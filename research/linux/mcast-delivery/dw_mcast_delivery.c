// SPDX-License-Identifier: MIT
/*
 * dw_mcast_delivery - real multicast delivery test for the V4-1 ip_add_membership
 * finding.  Runs as init in an isolated QEMU/initramfs guest.
 *
 * For each scenario the program forks a fresh child, creates fresh sockets,
 * configures lo (127.0.0.1/8, route_localnet=1 so a looped-back packet with a
 * 127.0.0.1 source survives ip_mc_validate_source), and for the injected
 * scenarios arms that child's /proc/self/task/<tid>/fail-nth immediately
 * before setsockopt(IP_ADD_MEMBERSHIP) or IP_ADD_SOURCE_MEMBERSHIP.
 *
 * It then proves delivery (or its absence) with a real UDP datagram: a second
 * socket sends to the group, the receiver socket bound to INADDR_ANY:port
 * reads it.  A unicast 127.0.0.1 datagram to the same receiver is the
 * network-configuration control, so a missing multicast datagram cannot be
 * blamed on a broken socket/route.
 *
 * Output is line oriented and timestamped with CLOCK_MONOTONIC and
 * CLOCK_REALTIME.  No output is produced between arming fail-nth and the
 * tested setsockopt, so the injected allocation count is not perturbed.
 */
#define _GNU_SOURCE
#include <arpa/inet.h>
#include <errno.h>
#include <fcntl.h>
#include <net/if.h>
#include <netinet/in.h>
#include <poll.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mount.h>
#include <sys/reboot.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <sys/syscall.h>
#include <sys/utsname.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#define MC_GROUP "239.1.2.3"
#define MC_HEX   "030201EF"	/* /proc/net/igmp host-order %08X */
#define MC_PORT  45678
#define MC_PORT2 45679

static int fail_fd = -1;
static int lo_ifindex = -1;

static void out(const char *fmt, ...)
{
	va_list ap;

	va_start(ap, fmt);
	vprintf(fmt, ap);
	va_end(ap);
	fflush(stdout);
}

static long long now_us(clockid_t clk)
{
	struct timespec ts;

	if (clock_gettime(clk, &ts) < 0)
		return -1;
	return (long long)ts.tv_sec * 1000000LL + ts.tv_nsec / 1000;
}

static int write_str(const char *path, const char *val)
{
	int fd = open(path, O_WRONLY);
	ssize_t n;

	if (fd < 0)
		return -1;
	n = write(fd, val, strlen(val));
	close(fd);
	return n == (ssize_t)strlen(val) ? 0 : -1;
}

static int read_file(const char *path, char *buf, size_t cap)
{
	int fd = open(path, O_RDONLY);
	ssize_t n;

	if (fd < 0)
		return -1;
	n = read(fd, buf, cap - 1);
	close(fd);
	if (n < 0)
		return -1;
	buf[n] = 0;
	return (int)n;
}

static int mkdir_if_needed(const char *path, mode_t mode)
{
	return (mkdir(path, mode) == 0 || errno == EEXIST) ? 0 : -1;
}

static void mount_all(void)
{
	mkdir_if_needed("/proc", 0555);
	if (mount("proc", "/proc", "proc", 0, ""))
		out("MOUNT_FAIL proc errno=%d\n", errno);
	mkdir_if_needed("/sys", 0555);
	if (mount("sysfs", "/sys", "sysfs", 0, ""))
		out("MOUNT_FAIL sysfs errno=%d\n", errno);
	mkdir_if_needed("/sys/kernel/debug", 0755);
	if (mount("debugfs", "/sys/kernel/debug", "debugfs", 0, ""))
		out("MOUNT_FAIL debugfs errno=%d\n", errno);
	mkdir_if_needed("/tmp", 01777);
	if (mount("tmpfs", "/tmp", "tmpfs", 0, "size=16m,mode=1777"))
		out("MOUNT_FAIL tmpfs errno=%d\n", errno);
}

static void knob(const char *path, const char *val)
{
	if (write_str(path, val) < 0)
		out("KNOB_FAIL path=%s val=%s errno=%d\n", path, val, errno);
}

static void config_fault_injection(void)
{
	const char *d = "/sys/kernel/debug";
	char p[256];

	snprintf(p, sizeof(p), "%s/failslab/ignore-gfp-wait", d);
	knob(p, "0");
	snprintf(p, sizeof(p), "%s/failslab/cache-filter", d);
	knob(p, "0");
	snprintf(p, sizeof(p), "%s/failslab/verbose", d);
	knob(p, "0");
	snprintf(p, sizeof(p), "%s/failslab/verbose_ratelimit_interval_ms", d);
	knob(p, "0");
	snprintf(p, sizeof(p), "%s/failslab/verbose_ratelimit_burst", d);
	knob(p, "100000");
}

static int open_fail_nth(void)
{
	char path[128];

	snprintf(path, sizeof(path), "/proc/self/task/%ld/fail-nth",
		 (long)syscall(SYS_gettid));
	fail_fd = open(path, O_RDWR);
	if (fail_fd < 0) {
		out("HARNESS_ERROR reason=open_fail_nth errno=%d path=%s\n",
		    errno, path);
		return -1;
	}
	return 0;
}

static int fn_set(unsigned int n)
{
	char b[16];
	int len = snprintf(b, sizeof(b), "%u", n);

	return pwrite(fail_fd, b, len, 0) == len ? 0 : -1;
}

static long fn_get(void)
{
	char b[32];
	ssize_t n;
	char *end;

	n = pread(fail_fd, b, sizeof(b) - 1, 0);
	if (n <= 0)
		return -1;
	b[n] = 0;
	errno = 0;
	return strtol(b, &end, 10);
}

/* ------------------------------------------------------------------ */
/* loopback setup                                                      */

static int net_setup(void)
{
	int fd;
	struct ifreq ifr;
	struct sockaddr_in *sin;

	fd = socket(AF_INET, SOCK_DGRAM, 0);
	if (fd < 0)
		return -1;

	memset(&ifr, 0, sizeof(ifr));
	snprintf(ifr.ifr_name, IFNAMSIZ, "lo");
	ifr.ifr_flags = IFF_UP | IFF_RUNNING;
	if (ioctl(fd, SIOCSIFFLAGS, &ifr) < 0)
		out("NET_FAIL op=up errno=%d\n", errno);

	memset(&ifr, 0, sizeof(ifr));
	snprintf(ifr.ifr_name, IFNAMSIZ, "lo");
	sin = (struct sockaddr_in *)&ifr.ifr_addr;
	sin->sin_family = AF_INET;
	sin->sin_addr.s_addr = htonl(INADDR_LOOPBACK);
	if (ioctl(fd, SIOCSIFADDR, &ifr) < 0)
		out("NET_FAIL op=addr errno=%d\n", errno);

	memset(&ifr, 0, sizeof(ifr));
	snprintf(ifr.ifr_name, IFNAMSIZ, "lo");
	sin = (struct sockaddr_in *)&ifr.ifr_netmask;
	sin->sin_family = AF_INET;
	sin->sin_addr.s_addr = htonl(0xff000000);
	if (ioctl(fd, SIOCSIFNETMASK, &ifr) < 0)
		out("NET_FAIL op=netmask errno=%d\n", errno);
	close(fd);

	lo_ifindex = (int)if_nametoindex("lo");
	if (lo_ifindex <= 0)
		out("NET_FAIL op=ifindex errno=%d\n", errno);

	/* A looped-back multicast packet carries saddr 127.0.0.1.  Without
	 * route_localnet, ip_mc_validate_source() drops it as IP_LOCALNET even
	 * when the group is joined.  Set it so delivery is governed by the
	 * group state, not by a default source-validation policy. */
	if (write_str("/proc/sys/net/ipv4/conf/lo/route_localnet", "1") < 0)
		out("NET_FAIL op=route_localnet errno=%d\n", errno);

	{
		char b[64];
		int n;

		b[0] = 0;
		n = read_file("/proc/sys/net/ipv4/conf/lo/route_localnet",
			      b, sizeof(b));
		out("NET lo_ifindex=%d lo_up=1 addr=127.0.0.1 route_localnet=%s",
		    lo_ifindex, n > 0 ? b : "?");
		if (n <= 0 || b[n - 1] != '\n')
			out("\n");
	}
	return lo_ifindex > 0 ? 0 : -1;
}

/* ------------------------------------------------------------------ */
/* socket helpers                                                      */

static int make_receiver(int port)
{
	int fd, one = 1;
	struct sockaddr_in a;

	fd = socket(AF_INET, SOCK_DGRAM, 0);
	if (fd < 0)
		return -1;
	setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &one, sizeof(one));
	memset(&a, 0, sizeof(a));
	a.sin_family = AF_INET;
	a.sin_addr.s_addr = htonl(INADDR_ANY);
	a.sin_port = htons(port);
	if (bind(fd, (struct sockaddr *)&a, sizeof(a)) < 0) {
		out("NET_FAIL op=bind port=%d errno=%d\n", port, errno);
		close(fd);
		return -1;
	}
	return fd;
}

static int make_sender(void)
{
	int fd;
	struct ip_mreqn m;
	unsigned char loop = 1, ttl = 1;

	fd = socket(AF_INET, SOCK_DGRAM, 0);
	if (fd < 0)
		return -1;
	memset(&m, 0, sizeof(m));
	inet_pton(AF_INET, "127.0.0.1", &m.imr_address);
	m.imr_ifindex = lo_ifindex;
	if (setsockopt(fd, IPPROTO_IP, IP_MULTICAST_IF, &m, sizeof(m)) < 0)
		out("NET_FAIL op=IP_MULTICAST_IF errno=%d\n", errno);
	if (setsockopt(fd, IPPROTO_IP, IP_MULTICAST_LOOP, &loop,
		       sizeof(loop)) < 0)
		out("NET_FAIL op=IP_MULTICAST_LOOP errno=%d\n", errno);
	if (setsockopt(fd, IPPROTO_IP, IP_MULTICAST_TTL, &ttl,
		       sizeof(ttl)) < 0)
		out("NET_FAIL op=IP_MULTICAST_TTL errno=%d\n", errno);
	return fd;
}

static long do_join(int fd, int ifindex, int *err)
{
	struct ip_mreqn m;
	long r;

	memset(&m, 0, sizeof(m));
	inet_pton(AF_INET, MC_GROUP, &m.imr_multiaddr);
	inet_pton(AF_INET, "127.0.0.1", &m.imr_address);
	m.imr_ifindex = ifindex;
	errno = 0;
	r = setsockopt(fd, IPPROTO_IP, IP_ADD_MEMBERSHIP, &m, sizeof(m));
	*err = errno;
	return r;
}

static long do_join_bad_group(int fd, int *err)
{
	struct ip_mreqn m;
	long r;

	memset(&m, 0, sizeof(m));
	inet_pton(AF_INET, "127.0.0.1", &m.imr_multiaddr);
	inet_pton(AF_INET, "127.0.0.1", &m.imr_address);
	errno = 0;
	r = setsockopt(fd, IPPROTO_IP, IP_ADD_MEMBERSHIP, &m, sizeof(m));
	*err = errno;
	return r;
}

static long do_join_source(int fd, int *err)
{
	struct ip_mreq_source m;
	long r;

	memset(&m, 0, sizeof(m));
	inet_pton(AF_INET, MC_GROUP, &m.imr_multiaddr);
	inet_pton(AF_INET, "127.0.0.1", &m.imr_sourceaddr);
	inet_pton(AF_INET, "127.0.0.1", &m.imr_interface);
	errno = 0;
	r = setsockopt(fd, IPPROTO_IP, IP_ADD_SOURCE_MEMBERSHIP, &m,
		       sizeof(m));
	*err = errno;
	return r;
}

static int msf_probe(int fd, int *err)
{
	struct ip_msfilter msf;
	socklen_t len = IP_MSFILTER_SIZE(0);

	memset(&msf, 0, sizeof(msf));
	inet_pton(AF_INET, MC_GROUP, &msf.imsf_multiaddr);
	inet_pton(AF_INET, "127.0.0.1", &msf.imsf_interface);
	msf.imsf_fmode = MCAST_INCLUDE;
	msf.imsf_numsrc = 0;
	errno = 0;
	if (getsockopt(fd, IPPROTO_IP, IP_MSFILTER, &msf, &len) < 0) {
		*err = errno;
		return -1;
	}
	return 0;
}

/* Return the Users value of MC_GROUP in /proc/net/igmp, -1 if absent. */
static void route_snapshot(void)
{
	static char buf[4096];
	int n, i;

	n = read_file("/proc/net/route", buf, sizeof(buf));
	if (n < 0) {
		out("ROUTE read_fail=1\n");
		return;
	}
	for (i = 0; i < n; i++)
		if (buf[i] == '\n')
			buf[i] = '|';
	out("ROUTE table=main snapshot=%s\n", buf);
}

static int igmp_users(void)
{
	static char buf[16384];
	char *p;
	int u, n;

	n = read_file("/proc/net/igmp", buf, sizeof(buf));
	if (n <= 0)
		return -1;
	p = strstr(buf, MC_HEX);
	if (!p)
		return -1;
	if (sscanf(p + strlen(MC_HEX), "%d", &u) != 1)
		return -1;
	return u;
}

static void igmp_snapshot(const char *tag)
{
	static char buf[8192];
	int n, i, users;

	n = read_file("/proc/net/igmp", buf, sizeof(buf));
	users = igmp_users();
	if (n < 0) {
		out("IGMP tag=%s read_fail=1\n", tag);
		return;
	}
	for (i = 0; i < n; i++)
		if (buf[i] == '\n')
			buf[i] = '|';
	out("IGMP tag=%s present=%d users=%d snapshot=%s\n",
	    tag, users >= 0, users, buf);
}

static void state_fd(const char *tag, int fd, long ret, int err, int injected,
		     long readback)
{
	int e = 0, msf = msf_probe(fd, &e);
	int users = igmp_users();

	out("STATE tag=%s ret=%ld errno=%d injected=%d readback=%ld "
	    "msf=%d msf_errno=%d igmp_users=%d",
	    tag, ret, err, injected, readback, msf, e, users);
	if (msf < 0)
		out(" msf_means=%s", e == EADDRNOTAVAIL ? "no_socket_membership" :
		    "probe_error");
	out("\n");
}

static int drain(int fd)
{
	char b[64];
	int n = 0;

	for (;;) {
		ssize_t r = recv(fd, b, sizeof(b), MSG_DONTWAIT);

		if (r < 0)
			break;
		n++;
	}
	return n;
}

static int recv_one(int fd, int timeout_ms, char *src, int *sport,
		    int *bytes)
{
	struct pollfd pfd = { .fd = fd, .events = POLLIN };
	struct sockaddr_in from;
	socklen_t fl = sizeof(from);
	char b[64];
	int pr;
	ssize_t r;

	pr = poll(&pfd, 1, timeout_ms);
	if (pr <= 0)
		return 0;
	memset(&from, 0, sizeof(from));
	r = recvfrom(fd, b, sizeof(b), 0, (struct sockaddr *)&from, &fl);
	if (r < 0)
		return 0;
	if (src)
		inet_ntop(AF_INET, &from.sin_addr, src, INET_ADDRSTRLEN);
	if (sport)
		*sport = ntohs(from.sin_port);
	if (bytes)
		*bytes = (int)r;
	return 1;
}

static void unicast_control(int fd, int s, int port, const char *tag)
{
	struct sockaddr_in d;
	char src[INET_ADDRSTRLEN] = "?";
	int sport = 0, bytes = 0, got;
	char payload[16];

	snprintf(payload, sizeof(payload), "U-%s", tag);
	memset(&d, 0, sizeof(d));
	d.sin_family = AF_INET;
	d.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
	d.sin_port = htons(port);
	drain(fd);
	errno = 0;
	{
		ssize_t r = sendto(s, payload, strlen(payload), 0,
				   (struct sockaddr *)&d, sizeof(d));

		out("UNICAST_SEND tag=%s ret=%zd errno=%d dst=127.0.0.1:%d\n",
		    tag, r, errno, port);
	}
	got = recv_one(fd, 1000, src, &sport, &bytes);
	out("UNICAST_RECV tag=%s got=%d src=%s:%d bytes=%d\n",
	    tag, got, src, sport, bytes);
	if (!got)
		out("CONTROL_FAIL tag=%s what=unicast\n", tag);
}

static void mcast_delivery(int fd, int s, int port, int count,
			   const char *tag)
{
	struct sockaddr_in d;
	int i, sent = 0, recvd = 0;

	memset(&d, 0, sizeof(d));
	d.sin_family = AF_INET;
	inet_pton(AF_INET, MC_GROUP, &d.sin_addr);
	d.sin_port = htons(port);
	drain(fd);
	for (i = 0; i < count; i++) {
		char payload[32];
		char src[INET_ADDRSTRLEN] = "?";
		int sport = 0, bytes = 0, got;
		ssize_t r;
		int e;

		snprintf(payload, sizeof(payload), "M-%s-%d", tag, i);
		errno = 0;
		r = sendto(s, payload, strlen(payload), 0,
			   (struct sockaddr *)&d, sizeof(d));
		e = errno;
		out("MCAST_SEND tag=%s seq=%d ret=%zd errno=%d dst=%s:%d\n",
		    tag, i, r, e, MC_GROUP, port);
		if (r < 0) {
			out("NET_FAIL tag=%s op=mcast_send errno=%d\n", tag, e);
			continue;
		}
		sent++;
		got = recv_one(fd, 300, src, &sport, &bytes);
		if (got) {
			recvd++;
			out("MCAST_RECV tag=%s seq=%d src=%s:%d bytes=%d\n",
			    tag, i, src, sport, bytes);
		} else {
			out("MCAST_RECV tag=%s seq=%d got=0\n", tag, i);
		}
	}
	out("DELIVERY tag=%s multicast_sent=%d multicast_recv=%d "
	    "expected=%d\n", tag, sent, recvd, count);
	if (recvd != count)
		out("DELIVERY_MISS tag=%s recv=%d expected=%d\n",
		    tag, recvd, count);
}

/* ------------------------------------------------------------------ */
/* scenarios (each runs in a fresh child)                              */

static void sc_normal(void)
{
	int fd = make_receiver(MC_PORT);
	int s = make_sender();

	out("SCENARIO normal begin\n");
	if (fd < 0 || s < 0) {
		out("SCENARIO normal setup_fail\n");
		return;
	}
	unicast_control(fd, s, MC_PORT, "normal_pre");
	{
		int e = 0;
		long r = do_join(fd, 0, &e);

		state_fd("normal_join", fd, r, e, 0, -1);
	}
	igmp_snapshot("normal_join");
	unicast_control(fd, s, MC_PORT, "normal_post");
	mcast_delivery(fd, s, MC_PORT, 4, "normal_joined");
	close(s);
	close(fd);
	out("SCENARIO normal end\n");
}

static void sc_n1(void)
{
	int fd = make_receiver(MC_PORT);
	int s = make_sender();
	long ret = -999, rb = -1;
	int err = 0, injected = 0;

	out("SCENARIO n1 begin\n");
	if (fd < 0 || s < 0)
		return;
	if (open_fail_nth() < 0)
		return;
	out("ARM tag=n1 n=1\n");
	if (fn_set(1) == 0) {
		injected = 1;
		errno = 0;
		ret = do_join(fd, 0, &err);
		rb = fn_get();
	}
	state_fd("n1", fd, ret, err, injected, rb);
	igmp_snapshot("n1");
	unicast_control(fd, s, MC_PORT, "n1");
	mcast_delivery(fd, s, MC_PORT, 2, "n1_no_join");
	{
		int e = 0;
		long r = do_join(fd, 0, &e);

		out("REJOIN tag=n1 ret=%ld errno=%d\n", r, e);
	}
	igmp_snapshot("n1_rejoin");
	mcast_delivery(fd, s, MC_PORT, 2, "n1_rejoined");
	close(s);
	close(fd);
	out("SCENARIO n1 end\n");
}

static void sc_n2(void)
{
	int fd = make_receiver(MC_PORT);
	int s = make_sender();
	long ret = -999, rb = -1;
	int err = 0, injected = 0;

	out("SCENARIO n2 begin\n");
	if (fd < 0 || s < 0)
		return;
	if (open_fail_nth() < 0)
		return;
	out("ARM tag=n2 n=2\n");
	if (fn_set(2) == 0) {
		injected = 1;
		errno = 0;
		ret = do_join(fd, 0, &err);
		rb = fn_get();
	}
	state_fd("n2", fd, ret, err, injected, rb);
	igmp_snapshot("n2");
	unicast_control(fd, s, MC_PORT, "n2");
	mcast_delivery(fd, s, MC_PORT, 4, "n2_after_inject");
	if (ret == 0) {
		int e = 0;
		long r;

		/* Socket membership was published.  Duplicate join is the
		 * documented EADDRINUSE path and does not call inc_group. */
		r = do_join(fd, 0, &e);
		out("DUPJOIN tag=n2 ret=%ld errno=%d\n", r, e);
		igmp_snapshot("n2_dupjoin");
		/* No query was sent; wait and show there is no spontaneous
		 * repair. */
		usleep(1000000);
		igmp_snapshot("n2_after_1s");
	} else {
		int e = 0;
		long r = do_join(fd, 0, &e);

		out("REJOIN tag=n2 ret=%ld errno=%d\n", r, e);
		igmp_snapshot("n2_rejoin");
		mcast_delivery(fd, s, MC_PORT, 2, "n2_rejoined");
	}
	close(s);
	close(fd);
	out("SCENARIO n2 end\n");
}

/*
 * Cross-socket refcount test.  The stock N=2 socket has a published
 * socket-side iml but never incremented the device-side users count.  A second
 * socket's successful join then creates the device group with users=1; closing
 * the first (failed-inc) socket decrements that group and removes it, breaking
 * the second socket's successful membership.
 */
static void sc_n2_cross(void)
{
	int fd = make_receiver(MC_PORT);
	int fd2 = make_receiver(MC_PORT2);
	int s = make_sender();
	long ret = -999, rb = -1;
	int err = 0, injected = 0;

	out("SCENARIO n2_cross begin\n");
	if (fd < 0 || fd2 < 0 || s < 0)
		return;
	if (open_fail_nth() < 0)
		return;
	out("ARM tag=n2_cross n=2\n");
	if (fn_set(2) == 0) {
		injected = 1;
		errno = 0;
		ret = do_join(fd, 0, &err);
		rb = fn_get();
	}
	state_fd("n2_cross", fd, ret, err, injected, rb);
	igmp_snapshot("n2_cross_pre_second");
	{
		int e = 0;
		long r = do_join(fd2, 0, &e);

		out("SECOND_JOIN tag=n2_cross ret=%ld errno=%d\n", r, e);
	}
	igmp_snapshot("n2_cross_second_joined");
	mcast_delivery(fd2, s, MC_PORT2, 2, "n2_cross_before_close");
	close(fd);
	usleep(200000);
	{
		int me = 0;
		int mr = msf_probe(fd2, &me);

		out("MSF tag=n2_cross_fd2_after_close_first rc=%d errno=%d\n",
		    mr, me);
	}
	igmp_snapshot("n2_cross_after_close_first");
	mcast_delivery(fd2, s, MC_PORT2, 2, "n2_cross_after_close");
	close(fd2);
	close(s);
	usleep(200000);
	igmp_snapshot("n2_cross_cleanup");
	out("SCENARIO n2_cross end\n");
}

static void sc_ssm_n2(void)
{
	int fd = make_receiver(MC_PORT);
	int s = make_sender();
	long ret = -999, rb = -1;
	int err = 0, injected = 0;

	out("SCENARIO ssm_n2 begin\n");
	if (fd < 0 || s < 0)
		return;
	if (open_fail_nth() < 0)
		return;
	out("ARM tag=ssm_n2 n=2\n");
	if (fn_set(2) == 0) {
		injected = 1;
		errno = 0;
		ret = do_join_source(fd, &err);
		rb = fn_get();
	}
	state_fd("ssm_n2", fd, ret, err, injected, rb);
	igmp_snapshot("ssm_n2");
	unicast_control(fd, s, MC_PORT, "ssm_n2");
	mcast_delivery(fd, s, MC_PORT, 2, "ssm_n2_after_inject");
	close(s);
	close(fd);
	out("SCENARIO ssm_n2 end\n");
}

static void sc_invalid(void)
{
	int fd = make_receiver(MC_PORT);
	int e = 0;
	long r;

	out("SCENARIO invalid begin\n");
	if (fd < 0)
		return;
	r = do_join_bad_group(fd, &e);
	state_fd("invalid_group", fd, r, e, 0, -1);
	igmp_snapshot("invalid_group");
	r = do_join(fd, 9999, &e);
	state_fd("invalid_ifindex", fd, r, e, 0, -1);
	igmp_snapshot("invalid_ifindex");
	close(fd);
	out("SCENARIO invalid end\n");
}

static void sc_two_sockets(void)
{
	int a = make_receiver(MC_PORT);
	int b = make_receiver(MC_PORT2);
	int s = make_sender();
	int e = 0;
	long r;

	out("SCENARIO two_sockets begin\n");
	if (a < 0 || b < 0 || s < 0)
		return;
	r = do_join(a, 0, &e);
	out("JOIN_A ret=%ld errno=%d\n", r, e);
	igmp_snapshot("two_sockets_a");
	r = do_join(b, 0, &e);
	out("JOIN_B ret=%ld errno=%d\n", r, e);
	igmp_snapshot("two_sockets_ab");
	r = do_join(a, 0, &e);
	out("DUPJOIN_A ret=%ld errno=%d\n", r, e);
	igmp_snapshot("two_sockets_dup");
	mcast_delivery(a, s, MC_PORT, 1, "two_sockets_a");
	mcast_delivery(b, s, MC_PORT2, 1, "two_sockets_b");
	close(b);
	usleep(200000);
	{
		int me = 0;
		int mr = msf_probe(a, &me);

		out("MSF tag=two_sockets_a_after_close_b rc=%d errno=%d\n",
		    mr, me);
	}
	igmp_snapshot("two_sockets_after_close_b");
	mcast_delivery(a, s, MC_PORT, 1, "two_sockets_after_close_b");
	close(a);
	close(s);
	usleep(200000);
	igmp_snapshot("two_sockets_cleanup");
	out("SCENARIO two_sockets end\n");
}

/* ------------------------------------------------------------------ */

struct scenario {
	const char *name;
	void (*fn)(void);
};

static const struct scenario scenarios[] = {
	{ "normal", sc_normal },
	{ "n1", sc_n1 },
	{ "n2", sc_n2 },
	{ "n2_cross", sc_n2_cross },
	{ "ssm_n2", sc_ssm_n2 },
	{ "invalid", sc_invalid },
	{ "two_sockets", sc_two_sockets },
};

static void run_scenario(const struct scenario *sc)
{
	pid_t p;
	int st;

	out("SCENARIO_BEGIN name=%s mono_us=%lld real_us=%lld\n",
	    sc->name, now_us(CLOCK_MONOTONIC), now_us(CLOCK_REALTIME));
	fflush(stdout);
	p = fork();
	if (p == 0) {
		sc->fn();
		fflush(stdout);
		_exit(0);
	}
	if (p < 0) {
		out("SCENARIO_ERROR name=%s what=fork errno=%d\n", sc->name, errno);
		return;
	}
	if (waitpid(p, &st, 0) < 0) {
		out("SCENARIO_ERROR name=%s what=waitpid errno=%d\n",
		    sc->name, errno);
		return;
	}
	usleep(200000);
	out("SCENARIO_END name=%s status=%d mono_us=%lld real_us=%lld "
	    "igmp_users=%d\n", sc->name, st, now_us(CLOCK_MONOTONIC),
	    now_us(CLOCK_REALTIME), igmp_users());
}

int main(void)
{
	struct utsname u;
	char cmdline[512] = "";
	int i;

	setvbuf(stdout, NULL, _IONBF, 0);
	mount_all();
	if (uname(&u) == 0)
		out("KERNEL sysname=%s release=%s version=%s\n",
		    u.sysname, u.release, u.version);
	if (read_file("/proc/cmdline", cmdline, sizeof(cmdline)) > 0) {
		char *nl = strchr(cmdline, '\n');

		if (nl)
			*nl = 0;
		out("CMDLINE %s\n", cmdline);
	}
	config_fault_injection();
	if (net_setup() < 0) {
		out("NET_FAIL setup_failed=1\n");
		out("MCAST_DONE\n");
		reboot(RB_POWER_OFF);
		for (;;)
			pause();
	}
	route_snapshot();
	for (i = 0; i < (int)(sizeof(scenarios) / sizeof(scenarios[0])); i++)
		run_scenario(&scenarios[i]);
	out("MCAST_DONE\n");
	fflush(stdout);
	sync();
	reboot(RB_POWER_OFF);
	for (;;)
		pause();
	return 0;
}
