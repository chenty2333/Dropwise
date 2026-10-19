// SPDX-License-Identifier: MIT
/*
 * dw_harness - prototype fault-injection harness for handoff atomicity of
 * Linux syscalls.
 *
 * Runs as init in a QEMU/initramfs VM with CONFIG_FAULT_INJECTION and the
 * per-task fail-nth interface.  For each target syscall P it:
 *
 *   1. calibrates two baselines on fresh objects, twice each:
 *        pre  - P is not executed
 *        post - P is executed without injection
 *   2. enumerates N = 1, 2, ...: write N to fail-nth, execute P, read
 *      fail-nth back (0 means the N-th should_fail() was injected), stop at
 *      the first N that is not injected;
 *   3. forks a survivor B that performs Q on the same shared object and
 *      reports what it saw;
 *   4. has the parent (independent observer) inspect the object by a path
 *      that is not the tested syscall (FIONREAD, fdinfo, /proc/locks, a
 *      second open+pread, a second epoll_wait);
 *   5. judges the observed state by P's return value: error -> pre,
 *      complete success -> post, short/partial -> the interface's partial
 *      semantics; anything else is a violation, an unstable calibration is
 *      unknown.
 *
 * Output is line oriented and unstably formatted on purpose: the host
 * runner only needs to grep it.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <pthread.h>
#include <signal.h>
#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/epoll.h>
#include <sys/ioctl.h>
#include <sys/mount.h>
#include <sys/reboot.h>
#include <sys/resource.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <netinet/in.h>
#include <netinet/tcp.h>
#include <arpa/inet.h>
#include <net/if.h>
#include <linux/tls.h>
#include <mqueue.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <sys/un.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#define MAX_OBS 1024
#define MAX_N 256
#define SURV_TIMEOUT_MS 3000

#ifndef FIONREAD
#define FIONREAD 0x541B
#endif

/* ------------------------------------------------------------------ */
/* small helpers                                                       */

static int fail_fd = -1;
static int kmsg_fd = -1;
static int verbose = 0;
static int trace_n = 0;

static void out(const char *fmt, ...)
{
	va_list ap;

	va_start(ap, fmt);
	vfprintf(stdout, fmt, ap);
	va_end(ap);
	fflush(stdout);
}

static void app(char *buf, size_t cap, const char *fmt, ...)
{
	size_t len = strlen(buf);
	va_list ap;

	if (len >= cap)
		return;
	va_start(ap, fmt);
	vsnprintf(buf + len, cap - len, fmt, ap);
	va_end(ap);
}

static void write_all(int fd, const void *buf, size_t len)
{
	const char *p = buf;

	while (len) {
		ssize_t n = write(fd, p, len);

		if (n < 0) {
			if (errno == EINTR)
				continue;
			_exit(1);
		}
		p += n;
		len -= n;
	}
}

static int read_full(int fd, void *buf, size_t len)
{
	char *p = buf;
	size_t got = 0;

	while (got < len) {
		ssize_t n = read(fd, p + got, len - got);

		if (n < 0) {
			if (errno == EINTR)
				continue;
			return -1;
		}
		if (n == 0)
			break;
		got += n;
	}
	return got == len ? 0 : -1;
}

/* ------------------------------------------------------------------ */
/* fail-nth                                                            */

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
/* mounts and fault-injection knobs                                    */

static int mkdir_if_needed(const char *path, mode_t mode)
{
	if (mkdir(path, mode) == 0 || errno == EEXIST)
		return 0;
	return -1;
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

static void knob(const char *path, const char *val)
{
	if (write_str(path, val) < 0)
		out("KNOB_FAIL path=%s val=%s errno=%d\n", path, val, errno);
}

static void config_fault_injection(void)
{
	const char *d = "/sys/kernel/debug";
	char p[256];
	const char *v = verbose ? "2" : "0";

	snprintf(p, sizeof(p), "%s/failslab/ignore-gfp-wait", d);
	knob(p, "0");
	snprintf(p, sizeof(p), "%s/failslab/cache-filter", d);
	knob(p, "0");
	snprintf(p, sizeof(p), "%s/failslab/verbose", d);
	knob(p, v);
	snprintf(p, sizeof(p), "%s/failslab/verbose_ratelimit_interval_ms", d);
	knob(p, "0");
	snprintf(p, sizeof(p), "%s/failslab/verbose_ratelimit_burst", d);
	knob(p, "100000");
	snprintf(p, sizeof(p), "%s/fail_page_alloc/ignore-gfp-wait", d);
	knob(p, "0");
	snprintf(p, sizeof(p), "%s/fail_page_alloc/ignore-gfp-highmem", d);
	knob(p, "0");
	snprintf(p, sizeof(p), "%s/fail_page_alloc/min-order", d);
	knob(p, "0");
	snprintf(p, sizeof(p), "%s/fail_page_alloc/verbose", d);
	knob(p, v);
	snprintf(p, sizeof(p), "%s/fail_page_alloc/verbose_ratelimit_interval_ms", d);
	knob(p, "0");
	snprintf(p, sizeof(p), "%s/fail_page_alloc/verbose_ratelimit_burst", d);
	knob(p, "100000");
}


/* ------------------------------------------------------------------ */
/* fail_function                                                       */

/* One fail_function injection point: an annotated kernel symbol and the
 * return value written to its retval file. */
struct ff_site {
	const char *func;
	long retval;
};

#define FF_DIR "/sys/kernel/debug/fail_function"
#define FF_INJECTABLE FF_DIR "/injectable"

static int ff_write(const char *name, const char *val)
{
	char p[256];

	snprintf(p, sizeof(p), "%s/%s", FF_DIR, name);
	return write_str(p, val);
}

static int ff_read_num(const char *name)
{
	char p[256], b[64];
	int fd, n;

	snprintf(p, sizeof(p), "%s/%s", FF_DIR, name);
	fd = open(p, O_RDONLY);
	if (fd < 0)
		return -1;
	n = read(fd, b, sizeof(b) - 1);
	close(fd);
	if (n <= 0)
		return -1;
	b[n] = 0;
	return atoi(b);
}

static int ff_config_base(void)
{
	if (access(FF_DIR "/inject", F_OK))
		return -1;
	ff_write("probability", "0");
	ff_write("interval", "1");
	ff_write("times", "1");
	ff_write("space", "0");
	ff_write("task-filter", "0");
	ff_write("verbose", "0");
	return 0;
}

/* Read the stock injectable list into buf; returns number of entries. */
static int ff_read_injectable(char *buf, size_t cap)
{
	int fd, n, lines = 0;
	char *p;

	fd = open(FF_INJECTABLE, O_RDONLY);
	if (fd < 0) {
		if (cap)
			buf[0] = 0;
		return -1;
	}
	n = 0;
	while (n < (int)cap - 1) {
		ssize_t r = read(fd, buf + n, cap - 1 - n);

		if (r <= 0)
			break;
		n += r;
	}
	close(fd);
	if (n < 0)
		n = 0;
	buf[n] = 0;
	for (p = buf; *p; p++)
		if (*p == '\n')
			lines++;
	return lines;
}

static int ff_register(const struct ff_site *site)
{
	char b[64];
	char p[256];
	int n;

	/* clear any previous injection points */
	ff_write("inject", "\n");
	errno = 0;
	if (write_str(FF_DIR "/inject", site->func) < 0) {
		out("FF_ERROR func=%s reason=register errno=%d\n", site->func,
		    errno);
		return -1;
	}
	snprintf(p, sizeof(p), "/sys/kernel/debug/fail_function/%s/retval",
		 site->func);
	snprintf(b, sizeof(b), "0x%lx", (unsigned long)site->retval);
	{
		if (write_str(p, b) < 0) {
			out("FF_ERROR func=%s reason=retval errno=%d val=%s\n",
			    site->func, errno, b);
			return -1;
		}
	}
	n = ff_read_num("times");
	out("FF_REGISTER func=%s retval=%s times=%d\n", site->func, b, n);
	return 0;
}

static void ff_unregister(const struct ff_site *site)
{
	char b[128];

	snprintf(b, sizeof(b), "!%s", site->func);
	ff_write("inject", b);
	ff_write("inject", "\n");
}

/* Arm one fail_function hit for the current task.  The injection is
 * one-shot (times=1) and task-filtered so unrelated tasks cannot consume
 * it.  make-it-fail is per thread. */
static int ff_arm_trial(void)
{
	/* One global shot.  task-filter is deliberately off: on this kernel
	 * a task that wrote its own make-it-fail=1 still did not match
	 * fail_task() from a kprobe handler (see RESULTS-round2.md).  The
	 * arm order matters: times is set last, immediately before P, so the
	 * debugfs writes themselves cannot consume the shot. */
	ff_write("interval", "1");
	ff_write("space", "0");
	ff_write("probability", "100");
	ff_write("times", "1");
	return 0;
}

static void ff_disarm_trial(void)
{
	ff_write("probability", "0");
}

/* ------------------------------------------------------------------ */
/* loopback network (for the TLS targets)                              */

static int net_setup(void)
{
	int fd = socket(AF_INET, SOCK_DGRAM, 0);
	struct ifreq ifr;
	struct sockaddr_in *sin;

	if (fd < 0)
		return -1;
	memset(&ifr, 0, sizeof(ifr));
	snprintf(ifr.ifr_name, IFNAMSIZ, "lo");
	ifr.ifr_flags = IFF_UP | IFF_RUNNING;
	if (ioctl(fd, SIOCSIFFLAGS, &ifr) < 0)
		out("NET_WARN reason=up errno=%d\n", errno);
	memset(&ifr, 0, sizeof(ifr));
	snprintf(ifr.ifr_name, IFNAMSIZ, "lo");
	sin = (struct sockaddr_in *)&ifr.ifr_addr;
	sin->sin_family = AF_INET;
	sin->sin_addr.s_addr = htonl(INADDR_LOOPBACK);
	if (ioctl(fd, SIOCSIFADDR, &ifr) < 0)
		out("NET_WARN reason=addr errno=%d\n", errno);
	memset(&ifr, 0, sizeof(ifr));
	snprintf(ifr.ifr_name, IFNAMSIZ, "lo");
	sin = (struct sockaddr_in *)&ifr.ifr_netmask;
	sin->sin_family = AF_INET;
	sin->sin_addr.s_addr = htonl(0xff000000);
	if (ioctl(fd, SIOCSIFNETMASK, &ifr) < 0)
		out("NET_WARN reason=netmask errno=%d\n", errno);
	close(fd);
	return 0;
}

static void ff_selftest(void)
{
	struct ff_site site = { "__x64_sys_getpid", -EINVAL };
	int t;

	if (ff_register(&site) < 0)
		return;
	ff_arm_trial();
	errno = 0;
	{
		long r = syscall(SYS_getpid);
		int e = errno;

		t = ff_read_num("times");
		out("FF_SELFTEST func=%s ret=%ld errno=%d times=%d\n",
		    site.func, r, e, t);
	}
	ff_disarm_trial();
	ff_write("times", "1");
	ff_unregister(&site);
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
	if (mount("tmpfs", "/tmp", "tmpfs", 0, "size=64m,mode=1777"))
		out("MOUNT_FAIL tmpfs errno=%d\n", errno);
}

/* ------------------------------------------------------------------ */
/* selftest: the milestone must stand on a working fail-nth           */

static int open_fail_nth(void)
{
	char path[128];

	if (fail_fd >= 0)
		close(fail_fd);
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

static int selftest(void)
{
	long v = -1;
	int n, sites = 0;

	if (open_fail_nth() < 0) {
		out("SELFTEST_FAIL reason=open_fail_nth\n");
		return -1;
	}
	if (fn_set(0) < 0) {
		out("SELFTEST_FAIL reason=write_zero errno=%d\n", errno);
		return -1;
	}

	/* The write of N and the readback must not consume the N-th fault. */
	if (fn_set(1) < 0 || (v = fn_get()) != 1) {
		out("SELFTEST_FAIL reason=set_get_consumed value=%ld errno=%d\n",
		    v, errno);
		fn_set(0);
		return -1;
	}
	fn_set(0);

	/* A syscall with no allocations must not consume a large N. */
	if (fn_set(1000) < 0 || syscall(SYS_getpid) <= 0 ||
	    (v = fn_get()) != 1000) {
		out("SELFTEST_FAIL reason=getpid_consumed value=%ld errno=%d\n",
		    v, errno);
		fn_set(0);
		return -1;
	}
	fn_set(0);

	/* Reproduce the documented fail-nth example on socketpair(). */
	for (n = 1; n <= 64; n++) {
		int sv[2], res, err;

		if (fn_set(n) < 0) {
			out("SELFTEST_FAIL reason=set n=%d errno=%d\n", n, errno);
			return -1;
		}
		errno = 0;
		res = socketpair(AF_UNIX, SOCK_STREAM, 0, sv);
		err = errno;
		if (!res) {
			close(sv[0]);
			close(sv[1]);
		}
		v = fn_get();
		out("SELFTEST_SOCKETPAIR n=%d injected=%d ret=%d errno=%d readback=%ld\n",
		    n, v == 0, res, res < 0 ? err : 0, v);
		if (v == 0)
			sites++;
		else
			break;
	}
	fn_set(0);
	if (!sites) {
		out("SELFTEST_FAIL reason=no_injection\n");
		return -1;
	}
	out("SELFTEST_OK socketpair_sites=%d\n", sites);
	return 0;
}

/* ------------------------------------------------------------------ */
/* target interface                                                    */

struct kid_report {
	long long rc;
	int err;
	int n;
	long long v[8];
};

enum dw_mech {
	M_FAILNTH  = 1 << 0,	/* /proc/<tid>/fail-nth allocation sites */
	M_SIGNAL   = 1 << 1,	/* SA_RESTART-less signal during a wait */
	M_FAILFUNC = 1 << 2,	/* /sys/kernel/debug/fail_function */
};

struct outcome {
	int mode;		/* 'P' pre, 'O' post, 'F' fault ... */
	int mech;
	long ret;
	int err;
	int injected;
	unsigned long readback;
	int hang;
	int got_kid;
	int signal_delay_us;
	int signal_rc;
	int sig_hit;
	int waited;
	struct kid_report kid;
	long long aux[8];
	char site[48];
	char obs[MAX_OBS];
};

struct target {
	const char *name;
	long full;		/* expected return value of a full success */
	unsigned mechs;		/* DW mechanisms this target supports */
	int (*setup)(void *ctx, char phase);
	long (*p)(void *ctx, int *err);
	void (*before_survivor)(void *ctx);
	void (*survivor)(void *ctx, int wfd);
	void (*observer)(void *ctx, const struct kid_report *kid,
			 struct outcome *o, char *obs, size_t cap);
	int (*partial_ok)(void *ctx, struct outcome *o);
	/* Optional interface-semantics predicates: legal error-side states
	 * that are not byte-identical to the pre baseline, and states where
	 * the survivor was told the object is unusable (OK_POISONED). */
	int (*error_ok)(void *ctx, struct outcome *o);
	int (*poisoned_ok)(void *ctx, struct outcome *o);
	/* A signal trial counts as an injected interruption only when the
	 * target says the wait was actually cut short. */
	int (*signal_hit)(void *ctx, struct outcome *o);
	/* fail_function candidates, tried one at a time. */
	const struct ff_site *ff;
	int nff;
	void (*teardown)(void *ctx);
};

union target_ctx {
	char pad[8192];
};

static union target_ctx C;

static void report(int wfd, long long rc, int err, long long v0, long long v1,
		   long long v2, long long v3, long long v4)
{
	struct kid_report r;

	memset(&r, 0, sizeof(r));
	r.rc = rc;
	r.err = err;
	r.n = 8;
	r.v[0] = v0;
	r.v[1] = v1;
	r.v[2] = v2;
	r.v[3] = v3;
	r.v[4] = v4;
	write_all(wfd, &r, sizeof(r));
}

/* ------------------------------------------------------------------ */
/* target: AF_UNIX sendmsg with SCM_RIGHTS                             */

struct scm_ctx {
	int sv[2];
	int filefd;
	unsigned long long ino;
};

static int scm_setup(void *p, char phase)
{
	struct scm_ctx *c = p;
	struct stat st;
	const char *path = "/tmp/dw_scm_file";

	unlink(path);
	c->filefd = open(path, O_RDWR | O_CREAT | O_TRUNC, 0600);
	if (c->filefd < 0)
		return -1;
	if (write(c->filefd, "hello", 5) != 5)
		return -1;
	if (fstat(c->filefd, &st))
		return -1;
	c->ino = (unsigned long long)st.st_ino;
	if (socketpair(AF_UNIX, SOCK_STREAM | SOCK_NONBLOCK, 0, c->sv))
		return -1;
	return 0;
}

static long scm_p(void *p, int *err)
{
	struct scm_ctx *c = p;
	char data = 'X';
	char cbuf[CMSG_SPACE(sizeof(int))];
	struct iovec iov = { .iov_base = &data, .iov_len = 1 };
	struct msghdr msg;
	struct cmsghdr *cm;
	long ret;

	memset(&msg, 0, sizeof(msg));
	memset(cbuf, 0, sizeof(cbuf));
	msg.msg_iov = &iov;
	msg.msg_iovlen = 1;
	msg.msg_control = cbuf;
	msg.msg_controllen = CMSG_SPACE(sizeof(int));
	cm = CMSG_FIRSTHDR(&msg);
	cm->cmsg_level = SOL_SOCKET;
	cm->cmsg_type = SCM_RIGHTS;
	cm->cmsg_len = CMSG_LEN(sizeof(int));
	memcpy(CMSG_DATA(cm), &c->filefd, sizeof(int));

	errno = 0;
	ret = sendmsg(c->sv[0], &msg, 0);
	*err = ret < 0 ? errno : 0;
	return ret;
}

static void scm_survivor(void *p, int wfd)
{
	struct scm_ctx *c = p;
	char data = 0;
	char cbuf[256];
	struct iovec iov = { .iov_base = &data, .iov_len = 1 };
	struct msghdr msg;
	struct cmsghdr *cm;
	struct stat st;
	int nfds = 0, ino_match = 0, ctrunc = 0;
	int fion = 0;
	long rc;
	int err;

	memset(&msg, 0, sizeof(msg));
	memset(cbuf, 0, sizeof(cbuf));
	msg.msg_iov = &iov;
	msg.msg_iovlen = 1;
	msg.msg_control = cbuf;
	msg.msg_controllen = sizeof(cbuf);
	errno = 0;
	rc = recvmsg(c->sv[1], &msg, 0);
	err = rc < 0 ? errno : 0;
	if (msg.msg_flags & MSG_CTRUNC)
		ctrunc = 1;
	for (cm = CMSG_FIRSTHDR(&msg); cm; cm = CMSG_NXTHDR(&msg, cm)) {
		if (cm->cmsg_level != SOL_SOCKET ||
		    cm->cmsg_type != SCM_RIGHTS)
			continue;
		if (cm->cmsg_len >= CMSG_LEN(sizeof(int))) {
			int fd;

			memcpy(&fd, CMSG_DATA(cm), sizeof(int));
			nfds++;
			if (!fstat(fd, &st) &&
			    (unsigned long long)st.st_ino == c->ino)
				ino_match = 1;
			close(fd);
		}
	}
	ioctl(c->sv[1], FIONREAD, &fion);
	report(wfd, rc, err, rc > 0 ? (unsigned char)data : -1, nfds,
	       ino_match, fion, ctrunc);
}

static void scm_observer(void *p, const struct kid_report *k,
			 struct outcome *o, char *obs, size_t cap)
{
	struct scm_ctx *c = p;
	int fion = 0;

	ioctl(c->sv[1], FIONREAD, &fion);
	app(obs, cap,
	    "recv_rc=%lld;recv_errno=%d;byte=%lld;nfds=%lld;ino_match=%lld;ctrunc=%lld;fion=%lld|fion_after=%d",
	    k->rc, k->err, k->v[0], k->v[1], k->v[2], k->v[4], k->v[3],
	    fion);
}

static void scm_teardown(void *p)
{
	struct scm_ctx *c = p;

	close(c->sv[0]);
	close(c->sv[1]);
	close(c->filefd);
	unlink("/tmp/dw_scm_file");
}

/* ------------------------------------------------------------------ */
/* target: pipe write (two pages)                                      */

#define PIPE_FULL 8192
#define PIPE_CAP  65536

struct pipe_ctx {
	int pfd[2];
	int fion_after;
	long read2;
	int read2_err;
};

static int pipe_setup(void *p, char phase)
{
	struct pipe_ctx *c = p;

	if (pipe2(c->pfd, O_NONBLOCK))
		return -1;
	(void)PIPE_CAP;
	return 0;
}

static long pipe_p(void *p, int *err)
{
	struct pipe_ctx *c = p;
	static char buf[PIPE_FULL];
	long ret;

	memset(buf, 'P', sizeof(buf));
	errno = 0;
	ret = write(c->pfd[1], buf, sizeof(buf));
	*err = ret < 0 ? errno : 0;
	return ret;
}

static void pipe_survivor(void *p, int wfd)
{
	struct pipe_ctx *c = p;
	char buf[PIPE_FULL * 2];
	long rc;
	int err, fion = 0, np = 0, first = -1, last = -1, i;

	memset(buf, 0, sizeof(buf));
	errno = 0;
	rc = read(c->pfd[0], buf, sizeof(buf));
	err = rc < 0 ? errno : 0;
	for (i = 0; i < rc; i++) {
		if (buf[i] == 'P')
			np++;
	}
	if (rc > 0) {
		first = (unsigned char)buf[0];
		last = (unsigned char)buf[rc - 1];
	}
	ioctl(c->pfd[0], FIONREAD, &fion);
	report(wfd, rc, err, np, first, last, fion, 0);
}

static void pipe_observer(void *p, const struct kid_report *k,
			  struct outcome *o, char *obs, size_t cap)
{
	struct pipe_ctx *c = p;
	char tmp[64];
	long rc2;

	c->fion_after = 0;
	ioctl(c->pfd[0], FIONREAD, &c->fion_after);
	errno = 0;
	rc2 = read(c->pfd[0], tmp, sizeof(tmp));
	c->read2 = rc2;
	c->read2_err = rc2 < 0 ? errno : 0;
	o->aux[0] = c->fion_after;
	o->aux[1] = c->read2;
	app(obs, cap,
	    "read_rc=%lld;read_errno=%d;np=%lld;first=%lld;last=%lld;fion=%lld|fion_after=%d;read2=%ld;read2_errno=%d",
	    k->rc, k->err, k->v[0], k->v[1], k->v[2], k->v[3],
	    c->fion_after, c->read2, c->read2_err);
}

static int pipe_partial(void *p, struct outcome *o)
{
	struct pipe_ctx *c = p;

	(void)c;
	return o->ret > 0 && o->ret < PIPE_FULL &&
	       o->kid.rc == o->ret && o->kid.v[0] == o->ret &&
	       o->kid.v[1] == 'P' && o->kid.v[2] == 'P' &&
	       o->kid.v[3] == 0 && o->aux[0] == 0 && o->aux[1] == -1;
}

static void pipe_teardown(void *p)
{
	struct pipe_ctx *c = p;

	close(c->pfd[0]);
	close(c->pfd[1]);
}

/* ------------------------------------------------------------------ */
/* target: tmpfs pwrite into a hole, survivor writes one byte          */

#define TMPFS_FULL 4096

struct tmpfs_ctx {
	int fd;
	unsigned long long ino;
	int s, pcnt, z;
	int first;
	long long size, blocks;
};

static int tmpfs_setup(void *p, char phase)
{
	struct tmpfs_ctx *c = p;
	struct stat st;

	unlink("/tmp/dw_tmpfs_file");
	c->fd = open("/tmp/dw_tmpfs_file", O_RDWR | O_CREAT | O_TRUNC, 0600);
	if (c->fd < 0)
		return -1;
	if (ftruncate(c->fd, 8192))
		return -1;
	if (fstat(c->fd, &st))
		return -1;
	c->ino = (unsigned long long)st.st_ino;
	return 0;
}

static long tmpfs_p(void *p, int *err)
{
	struct tmpfs_ctx *c = p;
	static char buf[TMPFS_FULL];
	long ret;

	memset(buf, 'P', sizeof(buf));
	errno = 0;
	ret = pwrite(c->fd, buf, sizeof(buf), 0);
	*err = ret < 0 ? errno : 0;
	return ret;
}

static void tmpfs_survivor(void *p, int wfd)
{
	struct tmpfs_ctx *c = p;
	long rc;
	int err;

	errno = 0;
	rc = pwrite(c->fd, "S", 1, 0);
	err = rc < 0 ? errno : 0;
	report(wfd, rc, err, 0, 0, 0, 0, 0);
}

static void tmpfs_observer(void *p, const struct kid_report *k,
			   struct outcome *o, char *obs, size_t cap)
{
	struct tmpfs_ctx *c = p;
	char buf[TMPFS_FULL];
	struct stat st;
	int fd, i;
	ssize_t n;

	fd = open("/tmp/dw_tmpfs_file", O_RDONLY);
	if (fd < 0) {
		app(obs, cap, "observer_open_errno=%d", errno);
		return;
	}
	memset(buf, 0, sizeof(buf));
	n = pread(fd, buf, sizeof(buf), 0);
	c->s = c->pcnt = c->z = 0;
	c->first = -1;
	for (i = 0; i < n; i++) {
		if (buf[i] == 'S')
			c->s++;
		else if (buf[i] == 'P')
			c->pcnt++;
		else if (buf[i] == 0)
			c->z++;
	}
	if (n > 0)
		c->first = (unsigned char)buf[0];
	c->size = c->blocks = -1;
	if (!fstat(fd, &st)) {
		c->size = (long long)st.st_size;
		c->blocks = (long long)st.st_blocks;
	}
	o->aux[0] = c->s;
	o->aux[1] = c->pcnt;
	o->aux[2] = c->z;
	o->aux[3] = c->first;
	close(fd);
	app(obs, cap,
	    "surv_rc=%lld;surv_errno=%d|S=%d;P=%d;Z=%d;first=%d;size=%lld;blocks=%lld;read=%zd",
	    k->rc, k->err, c->s, c->pcnt, c->z, c->first, c->size,
	    c->blocks, n);
}

static int tmpfs_partial(void *p, struct outcome *o)
{
	struct tmpfs_ctx *c = p;

	(void)c;
	return o->ret > 0 && o->ret < TMPFS_FULL &&
	       o->kid.rc == 1 && o->kid.err == 0 && o->aux[0] == 1 &&
	       o->aux[1] == o->ret - 1 &&
	       o->aux[2] == TMPFS_FULL - o->ret && o->aux[3] == 'S';
}

static void tmpfs_teardown(void *p)
{
	struct tmpfs_ctx *c = p;

	close(c->fd);
	unlink("/tmp/dw_tmpfs_file");
}

/* ------------------------------------------------------------------ */
/* target: epoll_ctl ADD                                               */

struct epoll_ctx {
	int epfd;
	int pfd[2];
	int tfd_count;
	unsigned int tfd_events;
	long wait2;
	unsigned int ev2;
	long del;
	int del_errno;
	char fdinfo[256];
};

static int epoll_setup(void *p, char phase)
{
	struct epoll_ctx *c = p;

	c->epfd = epoll_create1(0);
	if (c->epfd < 0)
		return -1;
	if (pipe2(c->pfd, O_NONBLOCK))
		return -1;
	return 0;
}

static long epoll_p(void *p, int *err)
{
	struct epoll_ctx *c = p;
	struct epoll_event ev;
	long ret;

	memset(&ev, 0, sizeof(ev));
	ev.events = EPOLLIN;
	ev.data.u64 = 0x1234;
	errno = 0;
	ret = epoll_ctl(c->epfd, EPOLL_CTL_ADD, c->pfd[0], &ev);
	*err = ret < 0 ? errno : 0;
	return ret;
}

static void epoll_before_survivor(void *p)
{
	struct epoll_ctx *c = p;

	if (write(c->pfd[1], "E", 1) != 1)
		out("HARNESS_WARN epoll_make_readable errno=%d\n", errno);
}

static void epoll_survivor(void *p, int wfd)
{
	struct epoll_ctx *c = p;
	struct epoll_event evs[8];
	long rc;
	int err;

	memset(evs, 0, sizeof(evs));
	errno = 0;
	rc = epoll_wait(c->epfd, evs, 8, 0);
	err = rc < 0 ? errno : 0;
	report(wfd, rc, err, rc > 0 ? (long long)evs[0].events : 0,
	       rc > 0 ? (long long)evs[0].data.u64 : 0,
	       rc > 1 ? (long long)evs[1].events : 0, 0, 0);
}

static void epoll_observer(void *p, const struct kid_report *k,
			   struct outcome *o, char *obs, size_t cap)
{
	struct epoll_ctx *c = p;
	char path[64], buf[4096];
	int fd, n, count = 0;
	char *s;
	unsigned int ev = 0;

	snprintf(path, sizeof(path), "/proc/self/fdinfo/%d", c->epfd);
	fd = open(path, O_RDONLY);
	n = -1;
	if (fd >= 0) {
		n = read(fd, buf, sizeof(buf) - 1);
		close(fd);
	}
	if (n > 0)
		buf[n] = 0;
	else
		buf[0] = 0;
	s = buf;
	while ((s = strstr(s, "tfd:")) != NULL) {
		count++;
		s += 4;
	}
	s = strstr(buf, "events:");
	if (s)
		sscanf(s + 7, "%x", &ev);
	c->tfd_count = count;
	c->tfd_events = ev;

	c->wait2 = 0;
	c->ev2 = 0;
	{
		struct epoll_event evs[8];

		memset(evs, 0, sizeof(evs));
		errno = 0;
		c->wait2 = epoll_wait(c->epfd, evs, 8, 0);
		if (c->wait2 > 0)
			c->ev2 = evs[0].events;
	}
	errno = 0;
	c->del = epoll_ctl(c->epfd, EPOLL_CTL_DEL, c->pfd[0], NULL);
	c->del_errno = c->del < 0 ? errno : 0;

	app(obs, cap,
	    "wait_rc=%lld;wait_errno=%d;ev=%lld;data=%lld|tfd=%d;tfd_ev=%x;wait2=%ld;ev2=%x;del=%ld;del_errno=%d",
	    k->rc, k->err, k->v[0], k->v[1], c->tfd_count, c->tfd_events,
	    c->wait2, c->ev2, c->del, c->del_errno);
	(void)n;
}

static void epoll_teardown(void *p)
{
	struct epoll_ctx *c = p;

	close(c->pfd[0]);
	close(c->pfd[1]);
	close(c->epfd);
}

/* ------------------------------------------------------------------ */
/* target: fcntl(F_SETLK) on a shared tmpfs file                       */

struct lock_ctx {
	int fd;
	unsigned long long ino;
	int locks;
	char type;
	int pidmatch;
};

static int lock_setup(void *p, char phase)
{
	struct lock_ctx *c = p;
	struct stat st;

	unlink("/tmp/dw_lock_file");
	c->fd = open("/tmp/dw_lock_file", O_RDWR | O_CREAT | O_TRUNC, 0600);
	if (c->fd < 0)
		return -1;
	if (ftruncate(c->fd, 1))
		return -1;
	if (fstat(c->fd, &st))
		return -1;
	c->ino = (unsigned long long)st.st_ino;
	return 0;
}

static long lock_p(void *p, int *err)
{
	struct lock_ctx *c = p;
	struct flock fl;
	long ret;

	memset(&fl, 0, sizeof(fl));
	fl.l_type = F_WRLCK;
	fl.l_whence = SEEK_SET;
	fl.l_start = 0;
	fl.l_len = 0;
	errno = 0;
	ret = fcntl(c->fd, F_SETLK, &fl);
	*err = ret < 0 ? errno : 0;
	return ret;
}

static void lock_survivor(void *p, int wfd)
{
	struct lock_ctx *c = p;
	struct flock fl;
	int fd;
	long rc;
	int err;

	fd = open("/tmp/dw_lock_file", O_RDWR);
	if (fd < 0) {
		report(wfd, -1, errno, 0, 0, 0, 0, 0);
		return;
	}
	memset(&fl, 0, sizeof(fl));
	fl.l_type = F_RDLCK;
	fl.l_whence = SEEK_SET;
	fl.l_start = 0;
	fl.l_len = 0;
	errno = 0;
	rc = fcntl(fd, F_SETLK, &fl);
	err = rc < 0 ? errno : 0;
	close(fd);
	(void)c;
	report(wfd, rc, err, 0, 0, 0, 0, 0);
}

static void lock_observer(void *p, const struct kid_report *k,
			  struct outcome *o, char *obs, size_t cap)
{
	struct lock_ctx *c = p;
	char buf[8192];
	char *save = NULL, *line;
	ssize_t n;
	int fd;

	fd = open("/proc/locks", O_RDONLY);
	n = -1;
	if (fd >= 0) {
		n = read(fd, buf, sizeof(buf) - 1);
		close(fd);
	}
	if (n > 0)
		buf[n] = 0;
	else
		buf[0] = 0;
	c->locks = 0;
	c->type = '?';
	c->pidmatch = 0;
	for (line = strtok_r(buf, "\n", &save); line;
	     line = strtok_r(NULL, "\n", &save)) {
		char *fields[12];
		char *sv = NULL, *tok;
		int nf = 0, pid;
		char *colon;
		unsigned long long ino;

		tok = strtok_r(line, " \t", &sv);
		while (tok && nf < 12) {
			fields[nf++] = tok;
			tok = strtok_r(NULL, " \t", &sv);
		}
		if (nf < 6)
			continue;
		colon = strrchr(fields[5], ':');
		if (!colon)
			continue;
		ino = strtoull(colon + 1, NULL, 10);
		if (ino != c->ino)
			continue;
		pid = atoi(fields[4]);
		if (pid != (int)getpid())
			continue;
		c->locks = 1;
		c->type = fields[3][0];
		c->pidmatch = 1;
		break;
	}
	app(obs, cap, "lock_rc=%lld;lock_errno=%d|locks=%d;type=%c;pidmatch=%d",
	    k->rc, k->err, c->locks, c->type, c->pidmatch);
}

static void lock_teardown(void *p)
{
	struct lock_ctx *c = p;

	close(c->fd);
	unlink("/tmp/dw_lock_file");
}

/* ------------------------------------------------------------------ */
/* target: accept4 on an AF_UNIX listening socket                      */
/*
 * A client is already queued before P, so a successful accept4 consumes the
 * pending connection.  The generic accept path allocates the new socket
 * before calling ->accept() and the struct file after it; an injected failure
 * in the latter therefore happens after the connection has been dequeued.
 */

static int accept_fd_max = 64;
static unsigned char accept_filled[4096];

struct accept_ctx {
	int lsock;
	int csock;
	int accepted_fd;
	int fill_to;
	int filled_to;		/* highest filler fd, 0 if none */
	char path[64];
};

static int accept_setup(void *p, char phase)
{
	struct accept_ctx *c = p;
	struct sockaddr_un sa;

	snprintf(c->path, sizeof(c->path), "/tmp/dw_accept.sock");
	unlink(c->path);
	c->lsock = socket(AF_UNIX, SOCK_STREAM | SOCK_NONBLOCK, 0);
	if (c->lsock < 0)
		return -1;
	memset(&sa, 0, sizeof(sa));
	sa.sun_family = AF_UNIX;
	snprintf(sa.sun_path, sizeof(sa.sun_path), "%s", c->path);
	if (bind(c->lsock, (struct sockaddr *)&sa, sizeof(sa)))
		return -1;
	if (listen(c->lsock, 4))
		return -1;
	c->csock = socket(AF_UNIX, SOCK_STREAM | SOCK_NONBLOCK, 0);
	if (c->csock < 0)
		return -1;
	if (connect(c->csock, (struct sockaddr *)&sa, sizeof(sa)))
		return -1;

	/*
	 * Fill the fd table to accept_fd_max - 1 so that accept4's own fd
	 * allocation has to expand it (alloc_fdtable -> kmalloc).  The
	 * expansion happens in FD_ADD, after ->accept() has already dequeued
	 * the connection.  accept_fd_max is doubled after each successful
	 * accept, because the fd table only grows.
	 */
	{
		struct rlimit rl;
		int i;

		c->filled_to = 0;
		c->fill_to = accept_fd_max - 1;
		if (!getrlimit(RLIMIT_NOFILE, &rl)) {
			if ((int)rl.rlim_max < accept_fd_max * 2) {
				rl.rlim_cur = rl.rlim_max = accept_fd_max * 2;
				setrlimit(RLIMIT_NOFILE, &rl);
			} else {
				rl.rlim_cur = rl.rlim_max;
				setrlimit(RLIMIT_NOFILE, &rl);
			}
		}
		memset(accept_filled, 0, sizeof(accept_filled));
		for (i = 3; i <= c->fill_to && i < (int)sizeof(accept_filled); i++) {
			if (fcntl(i, F_GETFD) != -1) {
				c->filled_to = i;
				continue;
			}
			if (dup2(0, i) < 0)
				break;
			accept_filled[i] = 1;
			c->filled_to = i;
		}
		if (c->filled_to != c->fill_to)
			out("HARNESS_WARN accept_fill target=%d got=%d errno=%d\n",
			    c->fill_to, c->filled_to, errno);
	}
	c->accepted_fd = -1;
	return 0;
}

static long accept_p(void *p, int *err)
{
	struct accept_ctx *c = p;
	long ret;

	errno = 0;
	ret = accept4(c->lsock, NULL, NULL, SOCK_NONBLOCK);
	*err = ret < 0 ? errno : 0;
	if (ret >= 0) {
		c->accepted_fd = (int)ret;
		if ((int)ret >= accept_fd_max)
			accept_fd_max = 2 * (int)ret;
	}
	return ret;
}

static void accept_survivor(void *p, int wfd)
{
	struct accept_ctx *c = p;
	int fd, wrote = 0, err;
	long rc;

	errno = 0;
	rc = accept4(c->lsock, NULL, NULL, SOCK_NONBLOCK);
	err = rc < 0 ? errno : 0;
	if (rc >= 0) {
		fd = (int)rc;
		if (write(fd, "A", 1) == 1)
			wrote = 1;
		close(fd);
	}
	report(wfd, rc >= 0 ? 1 : -1, err, wrote, 0, 0, 0, 0);
}

static void accept_observer(void *p, const struct kid_report *k,
			    struct outcome *o, char *obs, size_t cap)
{
	struct accept_ctx *c = p;
	char b = 0;
	ssize_t rr;
	int err, soerr = 0;
	socklen_t sl = sizeof(soerr);

	errno = 0;
	rr = recv(c->csock, &b, 1, MSG_DONTWAIT);
	err = rr < 0 ? errno : 0;
	getsockopt(c->csock, SOL_SOCKET, SO_ERROR, &soerr, &sl);
	app(obs, cap,
	    "accept_rc=%lld;accept_errno=%d;wrote=%lld|client_read=%zd;client_errno=%d;client_byte=%d;soerr=%d;accepted_open=%d",
	    k->rc, k->err, k->v[0], rr, err, rr > 0 ? (int)(unsigned char)b : -1,
	    soerr, c->accepted_fd >= 0);
}

static void accept_teardown(void *p)
{
	struct accept_ctx *c = p;
	int i;

	if (c->accepted_fd >= 0) {
		close(c->accepted_fd);
		c->accepted_fd = -1;
	}
	close(c->csock);
	close(c->lsock);
	for (i = 3; i < (int)sizeof(accept_filled); i++) {
		if (accept_filled[i]) {
			close(i);
			accept_filled[i] = 0;
		}
	}
	unlink(c->path);
}


/* ------------------------------------------------------------------ */
/* target: pipe write interrupted by a signal                          */
/*
 * Two scenarios.  "block" fills the pipe completely, so P blocks before
 * writing a byte and the only legal result is EINTR with no bytes queued.
 * "partial" leaves one page of space and P writes two pages, so it queues
 * the first page and then blocks; a signal must leave exactly that prefix
 * and return 8192, not an error.  This is the "already partially completed"
 * timing the first round could not reach with fail-nth.
 */

#define SIGPIPE_CAP 65536
#define SIGPIPE_BLOCK_LEN 8192
#define SIGPIPE_PART_LEN 16384
#define SIGPIPE_FREE 8192

struct sigpipe_ctx {
	int pfd[2];
	int capacity;
	int fill;
	int plen;
	int partial;
	int pcnt, fcnt, total;
	int first, last;
	int fion_after;
	long read2;
	int read2_err;
};

static int sigpipe_fill(struct sigpipe_ctx *c, int n)
{
	int fl, got = 0;
	char buf[8192];

	memset(buf, 'F', sizeof(buf));
	fl = fcntl(c->pfd[1], F_GETFL);
	if (fl < 0)
		return -1;
	fcntl(c->pfd[1], F_SETFL, fl | O_NONBLOCK);
	while (got < n) {
		int chunk = n - got > (int)sizeof(buf) ? (int)sizeof(buf) : n - got;
		ssize_t w = write(c->pfd[1], buf, chunk);

		if (w < 0) {
			if (errno == EINTR)
				continue;
			break;
		}
		if (w == 0)
			break;
		got += w;
	}
	fcntl(c->pfd[1], F_SETFL, fl);
	if (got != n)
		out("HARNESS_WARN target=pipe_sig reason=fill want=%d got=%d errno=%d\n",
		    n, got, errno);
	c->fill = got;
	return got == n ? 0 : -1;
}

static int sigpipe_setup(void *p, char phase)
{
	struct sigpipe_ctx *c = p;
	int fl;

	c->capacity = SIGPIPE_CAP;
	if (pipe2(c->pfd, 0))
		return -1;
	{
		int cap = fcntl(c->pfd[0], F_GETPIPE_SZ);

		if (cap > 0)
			c->capacity = cap;
	}
	c->plen = c->partial ? SIGPIPE_PART_LEN : SIGPIPE_BLOCK_LEN;
	if (phase == 'O' || phase == 'D') {
		c->fill = 0;
	} else {
		c->fill = c->capacity - (c->partial ? SIGPIPE_FREE : 0);
		if (sigpipe_fill(c, c->fill) < 0)
			return -1;
	}
	fl = fcntl(c->pfd[0], F_GETFL);
	if (fl >= 0)
		fcntl(c->pfd[0], F_SETFL, fl | O_NONBLOCK);
	return 0;
}

static long sigpipe_p(void *p, int *err)
{
	struct sigpipe_ctx *c = p;
	static char buf[SIGPIPE_PART_LEN];
	long ret;

	memset(buf, 'P', c->plen);
	errno = 0;
	ret = write(c->pfd[1], buf, c->plen);
	*err = ret < 0 ? errno : 0;
	return ret;
}

static void sigpipe_survivor(void *p, int wfd)
{
	struct sigpipe_ctx *c = p;
	char buf[131072];
	ssize_t rc;
	int err, fion = 0, i;

	memset(buf, 0, sizeof(buf));
	errno = 0;
	rc = read(c->pfd[0], buf, sizeof(buf));
	err = rc < 0 ? errno : 0;
	c->pcnt = c->fcnt = 0;
	c->first = c->last = -1;
	for (i = 0; i < rc; i++) {
		if (buf[i] == 'P')
			c->pcnt++;
		else if (buf[i] == 'F')
			c->fcnt++;
	}
	if (rc > 0) {
		c->first = (unsigned char)buf[0];
		c->last = (unsigned char)buf[rc - 1];
	}
	c->total = rc > 0 ? (int)rc : 0;
	ioctl(c->pfd[0], FIONREAD, &fion);
	report(wfd, rc, err, c->pcnt, c->fcnt, c->first, c->last, fion);
}

static void sigpipe_observer(void *p, const struct kid_report *k,
			     struct outcome *o, char *obs, size_t cap)
{
	struct sigpipe_ctx *c = p;
	char tmp[16];
	long rc2;

	c->fion_after = 0;
	ioctl(c->pfd[0], FIONREAD, &c->fion_after);
	errno = 0;
	rc2 = read(c->pfd[0], tmp, sizeof(tmp));
	c->read2 = rc2;
	c->read2_err = rc2 < 0 ? errno : 0;
	o->aux[0] = c->fion_after;
	o->aux[1] = c->read2;
	app(obs, cap,
	    "read_rc=%lld;read_errno=%d;total=%lld;pcnt=%lld;fcnt=%lld;first=%lld;last=%lld;fion=%lld|fion_after=%d;read2=%ld;read2_errno=%d",
	    k->rc, k->err, k->rc, k->v[0], k->v[1], k->v[2],
	    k->v[3], k->v[4], c->fion_after, c->read2, c->read2_err);
}

static int sigpipe_partial_ok(void *p, struct outcome *o)
{
	(void)p;
	/* The predicate runs in the worker, where C is the parent's copy:
	 * only o->kid/o->aux hold the trial child's observation. */
	return o->ret > 0 && o->ret < SIGPIPE_PART_LEN &&
	       o->kid.v[0] == o->ret &&
	       o->kid.v[1] == SIGPIPE_CAP - SIGPIPE_FREE &&
	       o->kid.v[2] == 'F' && o->kid.v[3] == 'P' &&
	       o->aux[0] == 0 && o->aux[1] == -1;
}

static int sigpipe_hit(void *p, struct outcome *o)
{
	struct sigpipe_ctx *c = p;

	if (o->ret < 0 && o->err == EINTR)
		return 1;
	if (c->partial && o->ret > 0 && o->ret < c->plen)
		return 1;
	return 0;
}

static void sigpipe_teardown(void *p)
{
	struct sigpipe_ctx *c = p;

	close(c->pfd[0]);
	close(c->pfd[1]);
}

/* ------------------------------------------------------------------ */
/* target: POSIX mq_send on a full queue interrupted by a signal       */
/*
 * mq_send(3) is atomic: either the message is enqueued (return 0) or the
 * call fails and nothing is enqueued.  The queue holds four messages; P
 * blocks on the fifth.  The survivor drains four, and the independent
 * observer looks at mq_curmsgs for a fifth (with its tag) that must not
 * exist on an EINTR.
 */

#define DWMQ_MAX 4
#define DWMQ_SIZE 64

struct mq_ctx {
	mqd_t mq;
	char name[64];
	int curmsgs;
	int extra;
	int extra_err;
};

static int mq_fill(struct mq_ctx *c, int n)
{
	int i;

	for (i = 0; i < n; i++) {
		char b[8];

		snprintf(b, sizeof(b), "F%d", i);
		if (mq_send(c->mq, b, 3, 2) < 0)
			return -1;
	}
	return 0;
}

static int mq_setup(void *p, char phase)
{
	struct mq_ctx *c = p;
	struct mq_attr attr;

	memset(&attr, 0, sizeof(attr));
	attr.mq_maxmsg = DWMQ_MAX;
	attr.mq_msgsize = DWMQ_SIZE;
	snprintf(c->name, sizeof(c->name), "/dw_mq_%d", (int)getpid());
	mq_unlink(c->name);
	c->mq = mq_open(c->name, O_CREAT | O_RDWR, 0600, &attr);
	if (c->mq == (mqd_t)-1) {
		out("HARNESS_WARN target=mq_send_sig reason=mq_open errno=%d\n",
		    errno);
		return -1;
	}
	if (mq_fill(c, phase == 'O' ? DWMQ_MAX - 1 : DWMQ_MAX) < 0) {
		out("HARNESS_WARN target=mq_send_sig reason=mq_fill errno=%d\n",
		    errno);
		return -1;
	}
	return 0;
}

static long mq_p(void *p, int *err)
{
	struct mq_ctx *c = p;
	long ret;

	errno = 0;
	ret = mq_send(c->mq, "P", 1, 1);
	*err = ret < 0 ? errno : 0;
	return ret;
}

static void mq_survivor(void *p, int wfd)
{
	struct mq_ctx *c = p;
	mqd_t q;
	struct mq_attr at;
	char buf[DWMQ_SIZE];
	char tags[8];
	int n = 0, i;

	tags[0] = 0;
	q = mq_open(c->name, O_RDONLY | O_NONBLOCK);
	if (q == (mqd_t)-1) {
		report(wfd, -1, errno, 0, 0, 0, 0, 0);
		return;
	}
	mq_getattr(q, &at);
	for (i = 0; i < DWMQ_MAX; i++) {
		ssize_t r = mq_receive(q, buf, sizeof(buf), NULL);

		if (r < 0)
			break;
		if (n < 4)
			tags[n] = buf[0];
		n++;
	}
	tags[n < 4 ? n : 4] = 0;
	mq_close(q);
	{
		long long packed = (long long)(unsigned char)tags[0] |
			((long long)(unsigned char)tags[1] << 8) |
			((long long)(unsigned char)tags[2] << 16) |
			((long long)(unsigned char)tags[3] << 24);

		report(wfd, n, 0, at.mq_curmsgs, packed, 0, 0, 0);
	}
}

static void mq_observer(void *p, const struct kid_report *k,
			struct outcome *o, char *obs, size_t cap)
{
	struct mq_ctx *c = p;
	mqd_t q;
	struct mq_attr at;
	char buf[DWMQ_SIZE];
	int extra = -1, extra_err = 0;

	q = mq_open(c->name, O_RDONLY | O_NONBLOCK);
	c->curmsgs = -1;
	if (q != (mqd_t)-1) {
		if (!mq_getattr(q, &at))
			c->curmsgs = (int)at.mq_curmsgs;
		errno = 0;
		if (mq_receive(q, buf, sizeof(buf), NULL) >= 0)
			extra = (unsigned char)buf[0];
		else
			extra_err = errno;
		mq_close(q);
	} else {
		extra_err = errno;
	}
	c->extra = extra;
	c->extra_err = extra_err;
	{
		long long p = k->v[1];
		char t[5];

		t[0] = (p & 0xff) ? (char)(p & 0xff) : '-';
		t[1] = ((p >> 8) & 0xff) ? (char)((p >> 8) & 0xff) : '-';
		t[2] = ((p >> 16) & 0xff) ? (char)((p >> 16) & 0xff) : '-';
		t[3] = ((p >> 24) & 0xff) ? (char)((p >> 24) & 0xff) : '-';
		t[4] = 0;
		app(obs, cap,
		    "qrecv_count=%lld;qrecv_errno=%d;tags=%s;qcurmsgs=%lld|curmsgs=%d;extra=%d;extra_errno=%d",
		    k->rc, k->err, t, k->v[0], c->curmsgs, c->extra,
		    c->extra_err);
	}
}

static int mq_hit(void *p, struct outcome *o)
{
	(void)p;
	return o->ret < 0 && o->err == EINTR;
}

static int mq_poisoned(void *p, struct outcome *o)
{
	(void)p;
	return o->kid.rc < 0 &&
	       (o->kid.err == EIO || o->kid.err == EBADF ||
		o->kid.err == ENOENT);
}

static void mq_teardown(void *p)
{
	struct mq_ctx *c = p;

	if (c->mq != (mqd_t)-1)
		mq_close(c->mq);
	mq_unlink(c->name);
}

/* ------------------------------------------------------------------ */
/* targets: kernel TLS on a TCP socket                                 */
/*
 * tls_tx: P = setsockopt(SOL_TLS, TLS_TX) on a socket whose ULP is "tls".
 * Contract (tls(7), setsockopt(2)): on failure the socket has no TX
 * context; the survivor's send must still put plaintext on the wire.  A
 * success is observable as a TLS record (0x17 0x03 0x03 ...) at the peer.
 * If P fails and the peer nevertheless sees a TLS record, the failed call
 * had a full effect.  If the survivor's send gets EIO/EBADMSG/EPIPE, the
 * kernel marked the object unusable: OK_POISONED.
 *
 * tls_rx: P = setsockopt(SOL_TLS, TLS_RX); the peer has a matching TX
 * context and sends one encrypted byte.  On P failure the survivor must
 * see raw ciphertext; a decrypted 'H' would mean a failed P installed the
 * receiving context.
 */

#define DW_TLS_KEY_A 0xa0
#define DW_TLS_KEY_B 0xb0

struct tls_ctx {
	int lsock;
	int peer;
	int target;
	int rx;			/* 1 = TLS_RX target */
	int ulp_ok;
	int send_rc;
	int send_err;
	int peer_n;
	int peer_err;
	int raw0, raw1, raw2;
	int rx_conf;
	int tx_conf;
	int peer_send_rc;
	int peer_send_err;
};

static void tls_make_info(struct tls12_crypto_info_aes_gcm_128 *ci, int seed)
{
	int i;

	memset(ci, 0, sizeof(*ci));
	ci->info.version = TLS_1_2_VERSION;
	ci->info.cipher_type = TLS_CIPHER_AES_GCM_128;
	for (i = 0; i < (int)sizeof(ci->key); i++)
		ci->key[i] = (unsigned char)(seed + i);
	ci->iv[7] = 1;
	ci->salt[3] = 1;
}

static int tcp_pair(struct tls_ctx *c)
{
	struct sockaddr_in sa;
	socklen_t sl = sizeof(sa);
	int one = 1;

	c->lsock = socket(AF_INET, SOCK_STREAM, 0);
	if (c->lsock < 0)
		return -1;
	memset(&sa, 0, sizeof(sa));
	sa.sin_family = AF_INET;
	sa.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
	sa.sin_port = 0;
	if (bind(c->lsock, (struct sockaddr *)&sa, sizeof(sa)) < 0)
		return -1;
	if (listen(c->lsock, 1) < 0)
		return -1;
	if (getsockname(c->lsock, (struct sockaddr *)&sa, &sl) < 0)
		return -1;
	c->target = socket(AF_INET, SOCK_STREAM, 0);
	if (c->target < 0)
		return -1;
	if (connect(c->target, (struct sockaddr *)&sa, sizeof(sa)) < 0)
		return -1;
	c->peer = accept(c->lsock, NULL, NULL);
	if (c->peer < 0)
		return -1;
	setsockopt(c->target, IPPROTO_TCP, TCP_NODELAY, &one, sizeof(one));
	setsockopt(c->peer, IPPROTO_TCP, TCP_NODELAY, &one, sizeof(one));
	fcntl(c->peer, F_SETFL, fcntl(c->peer, F_GETFL) | O_NONBLOCK);
	return 0;
}

static int tls_setup(void *p, char phase)
{
	struct tls_ctx *c = p;

	if (tcp_pair(c) < 0) {
		out("HARNESS_WARN target=tls reason=tcp_pair errno=%d\n", errno);
		return -1;
	}
	errno = 0;
	if (setsockopt(c->target, IPPROTO_TCP, TCP_ULP, "tls", 4) < 0) {
		out("HARNESS_WARN target=tls reason=tcp_ulp errno=%d\n", errno);
		return -1;
	}
	c->ulp_ok = 1;
	if (c->rx) {
		struct tls12_crypto_info_aes_gcm_128 ci;

		errno = 0;
		if (setsockopt(c->peer, IPPROTO_TCP, TCP_ULP, "tls", 4) < 0) {
			out("HARNESS_WARN target=tls_rx reason=peer_ulp errno=%d\n",
			    errno);
			return -1;
		}
		tls_make_info(&ci, DW_TLS_KEY_B);
		errno = 0;
		if (setsockopt(c->peer, SOL_TLS, TLS_TX, &ci, sizeof(ci)) < 0) {
			out("HARNESS_WARN target=tls_rx reason=peer_tx errno=%d\n",
			    errno);
			return -1;
		}
		fcntl(c->target, F_SETFL,
		      fcntl(c->target, F_GETFL) | O_NONBLOCK);
	}
	return 0;
}

static long tls_p(void *p, int *err)
{
	struct tls_ctx *c = p;
	struct tls12_crypto_info_aes_gcm_128 ci;
	long ret;

	if (c->rx)
		tls_make_info(&ci, DW_TLS_KEY_B);
	else
		tls_make_info(&ci, DW_TLS_KEY_A);
	errno = 0;
	ret = setsockopt(c->target, SOL_TLS, c->rx ? TLS_RX : TLS_TX,
			 &ci, sizeof(ci));
	*err = ret < 0 ? errno : 0;
	return ret;
}

static void tls_before_survivor(void *p)
{
	struct tls_ctx *c = p;

	if (!c->rx)
		return;
	errno = 0;
	c->peer_send_rc = send(c->peer, "H", 1, MSG_NOSIGNAL);
	c->peer_send_err = c->peer_send_rc < 0 ? errno : 0;
}

static void tls_survivor(void *p, int wfd)
{
	struct tls_ctx *c = p;
	char buf[64];
	ssize_t rc;
	int err = 0;

	memset(buf, 0, sizeof(buf));
	if (c->rx) {
		struct pollfd pf = { .fd = c->target, .events = POLLIN };

		poll(&pf, 1, 300);
		errno = 0;
		rc = recv(c->target, buf, 16, 0);
		err = rc < 0 ? errno : 0;
		report(wfd, rc, err, rc > 0 ? (unsigned char)buf[0] : -1,
		       rc > 1 ? (unsigned char)buf[1] : -1,
		       rc > 2 ? (unsigned char)buf[2] : -1, 0, 0);
		return;
	}
	errno = 0;
	rc = send(c->target, "P", 5, MSG_NOSIGNAL);
	err = rc < 0 ? errno : 0;
	report(wfd, rc, err, 0, 0, 0, 0, 0);
}

static void tls_observer(void *p, const struct kid_report *k,
			 struct outcome *o, char *obs, size_t cap)
{
	struct tls_ctx *c = p;
	int conf = 0;

	if (c->rx) {
		struct tls12_crypto_info_aes_gcm_128 ci;
		socklen_t len = sizeof(ci);

		errno = 0;
		c->rx_conf = getsockopt(c->target, SOL_TLS, TLS_RX, &ci,
					&len) == 0;
		o->aux[0] = c->rx_conf;
		o->aux[1] = c->peer_send_rc;
		app(obs, cap,
		    "recv_rc=%lld;recv_errno=%d;b0=%lld;b1=%lld;b2=%lld|peer_send_rc=%d;peer_send_errno=%d;rx_conf=%d",
		    k->rc, k->err, k->v[0], k->v[1], k->v[2],
		    c->peer_send_rc, c->peer_send_err, c->rx_conf);
		return;
	}
	{
		struct tls12_crypto_info_aes_gcm_128 ci;
		socklen_t len = sizeof(ci);
		struct pollfd pf = { .fd = c->peer, .events = POLLIN };
		char buf[64];
		int i;

		errno = 0;
		c->tx_conf = getsockopt(c->target, SOL_TLS, TLS_TX, &ci,
					&len) == 0;
		poll(&pf, 1, 300);
		c->peer_n = 0;
		c->peer_err = 0;
		for (i = 0; i < 8; i++) {
			ssize_t r = recv(c->peer, buf, sizeof(buf), 0);

			if (r <= 0) {
				if (r < 0 && errno != EAGAIN)
					c->peer_err = errno;
				break;
			}
			if (c->peer_n + r > 64)
				break;
			if (c->peer_n == 0) {
				c->raw0 = (unsigned char)buf[0];
				c->raw1 = r > 1 ? (unsigned char)buf[1] : -1;
				c->raw2 = r > 2 ? (unsigned char)buf[2] : -1;
			}
			c->peer_n += r;
		}
		conf = c->tx_conf;
		o->aux[0] = c->peer_n;
		o->aux[1] = c->tx_conf;
		app(obs, cap,
		    "send_rc=%lld;send_errno=%d|peer_n=%d;peer0=%d;peer1=%d;peer2=%d;peer_errno=%d;tx_conf=%d;plain=%d",
		    k->rc, k->err, c->peer_n, c->raw0, c->raw1, c->raw2,
		    c->peer_err, conf,
		    c->peer_n == 5 && c->raw0 == 'P');
	}
}

static int tls_poisoned(void *p, struct outcome *o)
{
	(void)p;
	if (o->aux[1] == 0 && o->aux[0] >= 0) {
		/* RX target: aux[1] is rx_conf; a failed RX with no context
		 * is not poison. */
	}
	if (o->kid.rc < 0 &&
	    (o->kid.err == EIO || o->kid.err == EBADMSG))
		return 1;
	if (o->kid.rc < 0 &&
	    (o->kid.err == EPIPE || o->kid.err == ECONNRESET ||
	     o->kid.err == ENOTCONN) && o->aux[0] == 0)
		return 1;
	return 0;
}

static void tls_teardown(void *p)
{
	struct tls_ctx *c = p;

	if (c->peer >= 0)
		close(c->peer);
	if (c->target >= 0)
		close(c->target);
	if (c->lsock >= 0)
		close(c->lsock);
}

/* ------------------------------------------------------------------ */
/* target: overlayfs copy-up triggered by open(O_RDWR)                 */
/*
 * Both layers live on tmpfs.  P opens a lower-only file O_RDWR, which
 * forces a copy-up.  open(2): on failure the file must not become
 * visible in upper; on success the whole file must be there.  The
 * survivor reopens the merged path and reads it; the observer reads the
 * upper directory directly (existence, length, first byte, mode) and
 * stats lower and merged.  A complete upper copy left behind by a failed
 * open is accepted as a legal pre-like state (the copy-up is an internal
 * step; the next user sees the old content and mode), but a partial upper
 * copy is not.
 */

struct ovl_ctx {
	char base[96];
	char lower[128];
	char upper[128];
	char work[128];
	char merged[128];
	char lfile[160];
	char ufile[160];
	char mfile[160];
	int mounted;
	int upper_exists;
	int upper_n;
	int upper_first;
	unsigned int upper_mode;
	unsigned int merged_mode;
	unsigned int lower_mode;
	long long lower_n;
};

static int write_file(const char *path, const char *data, size_t len)
{
	int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC, 0644);
	ssize_t n;

	if (fd < 0)
		return -1;
	n = write(fd, data, len);
	close(fd);
	return n == (ssize_t)len ? 0 : -1;
}

static void rm_tree(struct ovl_ctx *c)
{
	unlink(c->ufile);
	unlink(c->lfile);
	rmdir(c->merged);
	rmdir(c->work);
	rmdir(c->upper);
	rmdir(c->lower);
	rmdir(c->base);
}

static int ovl_setup(void *p, char phase)
{
	struct ovl_ctx *c = p;
	char opts[512];
	char *data;

	(void)phase;
	memset(c, 0, sizeof(*c));
	snprintf(c->base, sizeof(c->base), "/tmp/dw_ovl_%d", (int)getpid());
	snprintf(c->lower, sizeof(c->lower), "%s/lower", c->base);
	snprintf(c->upper, sizeof(c->upper), "%s/upper", c->base);
	snprintf(c->work, sizeof(c->work), "%s/work", c->base);
	snprintf(c->merged, sizeof(c->merged), "%s/merged", c->base);
	snprintf(c->lfile, sizeof(c->lfile), "%s/f", c->lower);
	snprintf(c->ufile, sizeof(c->ufile), "%s/f", c->upper);
	snprintf(c->mfile, sizeof(c->mfile), "%s/f", c->merged);

	if (mkdir(c->base, 0755) && errno != EEXIST)
		return -1;
	mkdir(c->lower, 0755);
	mkdir(c->upper, 0755);
	mkdir(c->work, 0755);
	mkdir(c->merged, 0755);
	data = malloc(4096);
	if (!data)
		return -1;
	memset(data, 'L', 4096);
	if (write_file(c->lfile, data, 4096) < 0) {
		free(data);
		return -1;
	}
	free(data);
	chmod(c->lfile, 0644);
	snprintf(opts, sizeof(opts), "lowerdir=%s,upperdir=%s,workdir=%s",
		 c->lower, c->upper, c->work);
	if (mount("overlay", c->merged, "overlay", 0, opts) < 0) {
		out("HARNESS_WARN target=overlay_copyup reason=mount errno=%d\n",
		    errno);
		return -1;
	}
	c->mounted = 1;
	return 0;
}

static long ovl_p(void *p, int *err)
{
	struct ovl_ctx *c = p;
	int fd;
	long ret;

	errno = 0;
	fd = open(c->mfile, O_RDWR);
	ret = fd >= 0 ? 0 : -1;
	if (fd >= 0)
		close(fd);
	*err = ret < 0 ? errno : 0;
	return ret;
}

static void ovl_survivor(void *p, int wfd)
{
	struct ovl_ctx *c = p;
	char buf[4096];
	struct stat st;
	int fd, i, lcnt = 0, first = -1;
	ssize_t n;
	int err = 0;

	errno = 0;
	fd = open(c->mfile, O_RDONLY);
	if (fd < 0) {
		report(wfd, -1, errno, 0, 0, 0, 0, 0);
		return;
	}
	n = read(fd, buf, sizeof(buf));
	if (n < 0)
		err = errno;
	for (i = 0; i < n; i++)
		if (buf[i] == 'L')
			lcnt++;
	if (n > 0)
		first = (unsigned char)buf[0];
	memset(&st, 0, sizeof(st));
	fstat(fd, &st);
	close(fd);
	report(wfd, n, err, n, lcnt, first,
	       (long long)st.st_mode & 07777, 0);
}

static void ovl_observer(void *p, const struct kid_report *k,
			 struct outcome *o, char *obs, size_t cap)
{
	struct ovl_ctx *c = p;
	struct stat st;
	int fd;

	c->upper_exists = 0;
	c->upper_n = -1;
	c->upper_first = -1;
	c->upper_mode = 0;
	fd = open(c->ufile, O_RDONLY);
	if (fd >= 0) {
		char buf[4096];
		ssize_t n;

		c->upper_exists = 1;
		memset(buf, 0, sizeof(buf));
		n = read(fd, buf, sizeof(buf));
		c->upper_n = n > 0 ? (int)n : 0;
		c->upper_first = n > 0 ? (unsigned char)buf[0] : -1;
		if (!fstat(fd, &st))
			c->upper_mode = st.st_mode & 07777;
		close(fd);
	}
	c->merged_mode = 0;
	if (!stat(c->mfile, &st))
		c->merged_mode = st.st_mode & 07777;
	c->lower_mode = 0;
	c->lower_n = -1;
	if (!stat(c->lfile, &st)) {
		c->lower_mode = st.st_mode & 07777;
		c->lower_n = (long long)st.st_size;
	}
	o->aux[0] = c->upper_exists;
	o->aux[1] = c->upper_n;
	o->aux[2] = c->upper_first;
	o->aux[3] = c->upper_mode;
	o->aux[4] = c->merged_mode;
	app(obs, cap,
	    "surv_rc=%lld;surv_errno=%d;surv_n=%lld;surv_L=%lld;surv_first=%lld;surv_mode=%lld|upper=%d;upper_n=%d;upper_first=%d;upper_mode=%o;merged_mode=%o;lower_mode=%o;lower_n=%lld",
	    k->rc, k->err, k->v[0], k->v[1], k->v[2], k->v[3],
	    c->upper_exists, c->upper_n, c->upper_first, c->upper_mode,
	    c->merged_mode, c->lower_mode, c->lower_n);
}

static int ovl_error_ok(void *p, struct outcome *o)
{
	(void)p;
	/* A fully populated upper file with the old mode is an internal
	 * copy-up artifact: the merged view still equals the pre state. */
	return o->aux[0] == 1 && o->aux[1] == 4096 &&
	       o->aux[2] == 'L' && o->aux[3] == 0644 &&
	       o->aux[4] == 0644 && o->kid.rc == 4096 &&
	       o->kid.v[1] == 4096 && o->kid.v[2] == 'L' &&
	       o->kid.v[3] == 0644;
}

static int ovl_poisoned(void *p, struct outcome *o)
{
	(void)p;
	return o->kid.rc < 0 && o->kid.err == EIO && o->aux[4] == 0;
}

static void ovl_teardown(void *p)
{
	struct ovl_ctx *c = p;

	if (c->mounted) {
		umount2(c->merged, MNT_DETACH);
		c->mounted = 0;
	}
	rm_tree(c);
}


static int sigpipe_block_setup(void *p, char phase)
{
	struct sigpipe_ctx *c = p;

	memset(c, 0, sizeof(*c));
	c->partial = 0;
	return sigpipe_setup(c, phase);
}

static int sigpipe_part_setup(void *p, char phase)
{
	struct sigpipe_ctx *c = p;

	memset(c, 0, sizeof(*c));
	c->partial = 1;
	return sigpipe_setup(c, phase);
}

static int mq_target_setup(void *p, char phase)
{
	struct mq_ctx *c = p;

	memset(c, 0, sizeof(*c));
	c->mq = (mqd_t)-1;
	return mq_setup(c, phase);
}

static int tls_tx_setup(void *p, char phase)
{
	struct tls_ctx *c = p;

	memset(c, 0, sizeof(*c));
	c->lsock = c->peer = c->target = -1;
	c->rx = 0;
	return tls_setup(c, phase);
}

static int tls_rx_setup(void *p, char phase)
{
	struct tls_ctx *c = p;

	memset(c, 0, sizeof(*c));
	c->lsock = c->peer = c->target = -1;
	c->rx = 1;
	return tls_setup(c, phase);
}

static const struct ff_site ff_tls[] = {
	{ "should_fail_net_realloc_skb", 1 },
};

static const struct ff_site ff_ovl[] = {
	{ "__filemap_add_folio", -ENOMEM },
};


/* ------------------------------------------------------------------ */
/* targets: EPOLLWAKEUP creation on ADD vs MOD                         */
/*
 * ep_insert() checks ep_create_wakeup_source() and removes the item on
 * failure.  ep_modify() sets epi->event.events (including EPOLLWAKEUP)
 * first and ignores the return value of ep_create_wakeup_source().
 * The target injects the wakeup_source_register() allocation failure:
 * MOD is expected to return 0 but leave no wakeup source, while ADD is
 * expected to fail and leave no item.  The observer reads fdinfo (mask)
 * and /sys/kernel/debug/wakeup_sources (registered sources).
 */

struct epwake_ctx {
	int epfd;
	int pfd[2];
	int add;
	int tfd_count;
	unsigned int tfd_events;
	int ws_total;
	int ws_epoll;
	int ws_other;
};

static int epwake_setup(void *p, char phase)
{
	struct epwake_ctx *c = p;
	struct epoll_event ev;

	(void)phase;
	c->epfd = epoll_create1(0);
	if (c->epfd < 0)
		return -1;
	if (pipe2(c->pfd, O_NONBLOCK))
		return -1;
	if (!c->add) {
		memset(&ev, 0, sizeof(ev));
		ev.events = EPOLLIN;
		ev.data.u64 = 0x77;
		if (epoll_ctl(c->epfd, EPOLL_CTL_ADD, c->pfd[0], &ev))
			return -1;
	}
	return 0;
}

static long epwake_p(void *p, int *err)
{
	struct epwake_ctx *c = p;
	struct epoll_event ev;
	long ret;

	memset(&ev, 0, sizeof(ev));
	ev.events = EPOLLIN | EPOLLWAKEUP;
	ev.data.u64 = 0x1234;
	errno = 0;
	ret = epoll_ctl(c->epfd, c->add ? EPOLL_CTL_ADD : EPOLL_CTL_MOD,
			c->pfd[0], &ev);
	*err = ret < 0 ? errno : 0;
	return ret;
}

static void epwake_before_survivor(void *p)
{
	struct epwake_ctx *c = p;

	if (write(c->pfd[1], "E", 1) != 1)
		out("HARNESS_WARN target=epwake reason=make_readable errno=%d\n",
		    errno);
}

static void epwake_survivor(void *p, int wfd)
{
	struct epwake_ctx *c = p;
	struct epoll_event evs[4];
	long rc;
	int err;

	memset(evs, 0, sizeof(evs));
	errno = 0;
	rc = epoll_wait(c->epfd, evs, 4, 0);
	err = rc < 0 ? errno : 0;
	report(wfd, rc, err, rc > 0 ? (long long)evs[0].events : 0,
	       rc > 0 ? (long long)evs[0].data.u64 : 0, 0, 0, 0);
}

static void epwake_observer(void *p, const struct kid_report *k,
			    struct outcome *o, char *obs, size_t cap)
{
	struct epwake_ctx *c = p;
	char path[64], buf[8192];
	int fd, n, count = 0, ws_total = 0, ws_ep = 0, ws_other = 0;
	char *s, *save = NULL, *line;
	unsigned int ev = 0;

	snprintf(path, sizeof(path), "/proc/self/fdinfo/%d", c->epfd);
	fd = open(path, O_RDONLY);
	n = -1;
	if (fd >= 0) {
		n = read(fd, buf, sizeof(buf) - 1);
		close(fd);
	}
	if (n > 0)
		buf[n] = 0;
	else
		buf[0] = 0;
	s = buf;
	while ((s = strstr(s, "tfd:")) != NULL) {
		count++;
		s += 4;
	}
	s = strstr(buf, "events:");
	if (s)
		sscanf(s + 7, "%x", &ev);
	c->tfd_count = count;
	c->tfd_events = ev;

	fd = open("/sys/kernel/debug/wakeup_sources", O_RDONLY);
	n = -1;
	if (fd >= 0) {
		n = read(fd, buf, sizeof(buf) - 1);
		close(fd);
	}
	if (n > 0) {
		buf[n] = 0;
		for (line = strtok_r(buf, "\n", &save); line;
		     line = strtok_r(NULL, "\n", &save)) {
			char *name = line;
			char *sp;

			while (*name == ' ' || *name == '\t')
				name++;
			sp = name;
			while (*sp && *sp != ' ' && *sp != '\t')
				sp++;
			*sp = 0;
			if (!strcmp(name, "name") || !name[0])
				continue;
			ws_total++;
			if (!strcmp(name, "eventpoll"))
				ws_ep++;
			else
				ws_other++;
		}
	}
	c->ws_total = ws_total;
	c->ws_epoll = ws_ep;
	c->ws_other = ws_other;
	o->aux[0] = c->tfd_count;
	o->aux[1] = c->tfd_events;
	o->aux[2] = c->ws_total;
	o->aux[3] = c->ws_epoll;
	o->aux[4] = c->ws_other;
	app(obs, cap,
	    "wait_rc=%lld;wait_errno=%d;wait_ev=%lld;wait_data=%lld|tfd=%d;tfd_ev=%x;ws_total=%d;ws_ep=%d;ws_other=%d",
	    k->rc, k->err, k->v[0], k->v[1], c->tfd_count, c->tfd_events,
	    c->ws_total, c->ws_epoll, c->ws_other);
}

static int epwake_add_error_ok(void *p, struct outcome *o)
{
	(void)p;
	/* ep_insert() removed the item before returning the error.  The
	 * per-epoll "eventpoll" wakeup source may stay cached; that does
	 * not change the interest list. */
	return o->aux[0] == 0 && o->kid.rc == 0;
}

static void epwake_teardown(void *p)
{
	struct epwake_ctx *c = p;

	close(c->pfd[0]);
	close(c->pfd[1]);
	close(c->epfd);
}

static int epwake_add_setup(void *p, char phase)
{
	struct epwake_ctx *c = p;

	memset(c, 0, sizeof(*c));
	c->add = 1;
	return epwake_setup(c, phase);
}

static int epwake_mod_setup(void *p, char phase)
{
	struct epwake_ctx *c = p;

	memset(c, 0, sizeof(*c));
	c->add = 0;
	return epwake_setup(c, phase);
}

/* ------------------------------------------------------------------ */
/* targets table                                                       */

static const struct target t_scm = {
	.name = "scm_rights",
	.full = 1,
	.mechs = M_FAILNTH,
	.setup = scm_setup,
	.p = scm_p,
	.survivor = scm_survivor,
	.observer = scm_observer,
	.teardown = scm_teardown,
};

static const struct target t_pipe = {
	.name = "pipe_write",
	.full = PIPE_FULL,
	.mechs = M_FAILNTH,
	.setup = pipe_setup,
	.p = pipe_p,
	.survivor = pipe_survivor,
	.observer = pipe_observer,
	.partial_ok = pipe_partial,
	.teardown = pipe_teardown,
};

static const struct target t_tmpfs = {
	.name = "tmpfs_pwrite",
	.full = TMPFS_FULL,
	.mechs = M_FAILNTH,
	.setup = tmpfs_setup,
	.p = tmpfs_p,
	.survivor = tmpfs_survivor,
	.observer = tmpfs_observer,
	.partial_ok = tmpfs_partial,
	.teardown = tmpfs_teardown,
};

static const struct target t_epoll = {
	.name = "epoll_ctl_add",
	.full = 0,
	.mechs = M_FAILNTH,
	.setup = epoll_setup,
	.p = epoll_p,
	.before_survivor = epoll_before_survivor,
	.survivor = epoll_survivor,
	.observer = epoll_observer,
	.teardown = epoll_teardown,
};

static const struct target t_lock = {
	.name = "fcntl_setlk",
	.full = 0,
	.mechs = M_FAILNTH,
	.setup = lock_setup,
	.p = lock_p,
	.survivor = lock_survivor,
	.observer = lock_observer,
	.teardown = lock_teardown,
};

static const struct target t_accept = {
	.name = "accept4_unix",
	.full = -2,
	.mechs = M_FAILNTH,	/* success is any non-negative fd */
	.setup = accept_setup,
	.p = accept_p,
	.survivor = accept_survivor,
	.observer = accept_observer,
	.teardown = accept_teardown,
};

static const struct target t_pipe_sig_block = {
	.name = "pipe_sig_block",
	.full = SIGPIPE_BLOCK_LEN,
	.mechs = M_SIGNAL,
	.setup = sigpipe_block_setup,
	.p = sigpipe_p,
	.survivor = sigpipe_survivor,
	.observer = sigpipe_observer,
	.signal_hit = sigpipe_hit,
	.teardown = sigpipe_teardown,
};

static const struct target t_pipe_sig_part = {
	.name = "pipe_sig_partial",
	.full = SIGPIPE_PART_LEN,
	.mechs = M_SIGNAL,
	.setup = sigpipe_part_setup,
	.p = sigpipe_p,
	.survivor = sigpipe_survivor,
	.observer = sigpipe_observer,
	.partial_ok = sigpipe_partial_ok,
	.signal_hit = sigpipe_hit,
	.teardown = sigpipe_teardown,
};

static const struct target t_mq_send = {
	.name = "mq_send_full",
	.full = 0,
	.mechs = M_SIGNAL,
	.setup = mq_target_setup,
	.p = mq_p,
	.survivor = mq_survivor,
	.observer = mq_observer,
	.poisoned_ok = mq_poisoned,
	.signal_hit = mq_hit,
	.teardown = mq_teardown,
};

static const struct target t_tls_tx = {
	.name = "tls_tx",
	.full = 0,
	.mechs = M_FAILNTH | M_FAILFUNC,
	.setup = tls_tx_setup,
	.p = tls_p,
	.survivor = tls_survivor,
	.observer = tls_observer,
	.poisoned_ok = tls_poisoned,
	.ff = ff_tls,
	.nff = (int)(sizeof(ff_tls) / sizeof(ff_tls[0])),
	.teardown = tls_teardown,
};

static const struct target t_tls_rx = {
	.name = "tls_rx",
	.full = 0,
	.mechs = M_FAILNTH | M_FAILFUNC,
	.setup = tls_rx_setup,
	.p = tls_p,
	.before_survivor = tls_before_survivor,
	.survivor = tls_survivor,
	.observer = tls_observer,
	.poisoned_ok = tls_poisoned,
	.ff = ff_tls,
	.nff = (int)(sizeof(ff_tls) / sizeof(ff_tls[0])),
	.teardown = tls_teardown,
};

static const struct target t_ovl = {
	.name = "overlay_copyup",
	.full = 0,
	.mechs = M_FAILNTH | M_FAILFUNC,
	.setup = ovl_setup,
	.p = ovl_p,
	.survivor = ovl_survivor,
	.observer = ovl_observer,
	.error_ok = ovl_error_ok,
	.poisoned_ok = ovl_poisoned,
	.ff = ff_ovl,
	.nff = (int)(sizeof(ff_ovl) / sizeof(ff_ovl[0])),
	.teardown = ovl_teardown,
};

static const struct target t_epwake_mod = {
	.name = "epoll_mod_wakeup",
	.full = 0,
	.mechs = M_FAILNTH,
	.setup = epwake_mod_setup,
	.p = epwake_p,
	.before_survivor = epwake_before_survivor,
	.survivor = epwake_survivor,
	.observer = epwake_observer,
	.teardown = epwake_teardown,
};

static const struct target t_epwake_add = {
	.name = "epoll_add_wakeup",
	.full = 0,
	.mechs = M_FAILNTH,
	.setup = epwake_add_setup,
	.p = epwake_p,
	.before_survivor = epwake_before_survivor,
	.survivor = epwake_survivor,
	.observer = epwake_observer,
	.error_ok = epwake_add_error_ok,
	.teardown = epwake_teardown,
};

static const struct target *all_targets[] = {
	&t_scm, &t_pipe, &t_tmpfs, &t_epoll, &t_lock, &t_accept,
	&t_tls_tx, &t_tls_rx, &t_ovl, &t_mq_send,
	&t_pipe_sig_block, &t_pipe_sig_part,
	&t_epwake_mod, &t_epwake_add, NULL
};

/* ------------------------------------------------------------------ */
/* trial execution                                                     */

static void kmsg_mark(const char *tag, const char *name, unsigned int n)
{
	if (kmsg_fd < 0)
		return;
	dprintf(kmsg_fd, "%s target=%s n=%u\n", tag, name, n);
}

static int run_survivor(const struct target *t, int rp[2], struct outcome *o)
{
	pid_t pid;
	struct pollfd pfd;
	int pr, status;

	if (t->before_survivor)
		t->before_survivor(&C);
	pid = fork();
	if (pid < 0) {
		out("HARNESS_ERROR target=%s reason=fork errno=%d\n", t->name,
		    errno);
		return -1;
	}
	if (pid == 0) {
		close(rp[0]);
		signal(SIGPIPE, SIG_IGN);
		t->survivor(&C, rp[1]);
		close(rp[1]);
		_exit(0);
	}
	close(rp[1]);
	rp[1] = -1;
	memset(&pfd, 0, sizeof(pfd));
	pfd.fd = rp[0];
	pfd.events = POLLIN;
	pr = poll(&pfd, 1, SURV_TIMEOUT_MS);
	if (pr > 0) {
		if (read_full(rp[0], &o->kid, sizeof(o->kid)) == 0)
			o->got_kid = 1;
	} else {
		o->hang = 1;
		kill(pid, SIGKILL);
	}
	close(rp[0]);
	rp[0] = -1;
	waitpid(pid, &status, 0);
	if (!o->hang && !o->got_kid)
		o->hang = 1;
	if (o->hang) {
		app(o->obs, sizeof(o->obs), "survivor_hang=1");
		return -1;
	}
	t->observer(&C, &o->kid, o, o->obs, sizeof(o->obs));
	return 1;
}

static int run_trial(const struct target *t, int mode, unsigned int n,
		     int mech, struct outcome *o)
{
	int rp[2] = { -1, -1 };
	int ret = -1;

	memset(o, 0, sizeof(*o));
	o->mode = mode;
	o->mech = mech;
	fn_set(0);
	ff_write("probability", "0");
	/*
	 * The report pipe is created before the target setup and before
	 * fail-nth is armed, so it cannot perturb the target's allocation
	 * sequence or consume an injection point.
	 */
	if (pipe(rp)) {
		out("HARNESS_ERROR target=%s reason=report_pipe errno=%d\n",
		    t->name, errno);
		return -1;
	}
	if (t->setup(&C, (char)mode) < 0) {
		out("HARNESS_ERROR target=%s reason=setup phase=%c errno=%d\n",
		    t->name, mode, errno);
		goto out;
	}

	if (mode != 'P') {
		if (mode == 'D' && !(t->mechs & (M_FAILNTH | M_FAILFUNC))) {
			ret = 1;
			goto out;
		}
		if (mode == 'F' || mode == 'R') {
			if (verbose)
				kmsg_mark("DWTRACE", t->name, n);
			if (fn_set(n) < 0) {
				out("HARNESS_ERROR target=%s reason=fn_set n=%u errno=%d\n",
				    t->name, n, errno);
				goto out;
			}
		} else if (mode == 'G') {
			if (ff_arm_trial() < 0)
				goto out;
		}
		errno = 0;
		o->ret = t->p(&C, &o->err);
		if (o->ret < 0 && o->err == 0)
			o->err = errno;
		if (mode == 'D') {
			ret = 1;
			goto out;
		}
		if (mode == 'F' || mode == 'R') {
			long rb = fn_get();

			if (rb < 0) {
				out("HARNESS_ERROR target=%s reason=fn_get n=%u\n",
				    t->name, n);
				goto out;
			}
			o->readback = (unsigned long)rb;
			o->injected = (rb == 0);
			if (mode == 'R') {
				fn_set(0);
				ret = 1;
				goto out;
			}
			if (!o->injected) {
				fn_set(0);
				ret = 0;
				goto out;
			}
		}
		if (mode == 'G') {
			int times = ff_read_num("times");

			o->readback = times < 0 ? 999 : (unsigned long)times;
			o->injected = (times == 0);
			ff_disarm_trial();
			if (!o->injected) {
				ret = 0;
				goto out;
			}
		}
	} else {
		o->ret = 0;
		o->err = 0;
	}

	/* The survivor must never inherit an armed fail_nth. */
	fn_set(0);
	if (mode != 'G')
		ff_write("probability", "0");
	ret = run_survivor(t, rp, o);
out:
	if (rp[0] >= 0)
		close(rp[0]);
	if (rp[1] >= 0)
		close(rp[1]);
	ff_write("probability", "0");
	t->teardown(&C);
	return ret;
}

/* ------------------------------------------------------------------ */
/* signal interruption                                                 */

struct p_thread_arg {
	const struct target *t;
	long ret;
	int err;
	int started;
	int tid;
};

static void *p_thread_fn(void *x)
{
	struct p_thread_arg *a = x;

	a->tid = (int)syscall(SYS_gettid);
	a->started = 1;
	errno = 0;
	a->ret = a->t->p(&C, &a->err);
	if (a->ret < 0 && a->err == 0)
		a->err = errno;
	return NULL;
}

static void dw_sig_handler(int sig)
{
	(void)sig;
}

static void install_sig_handler(void)
{
	struct sigaction sa;
	sigset_t set;

	memset(&sa, 0, sizeof(sa));
	sa.sa_handler = dw_sig_handler;
	sigemptyset(&sa.sa_mask);
	sa.sa_flags = 0;	/* deliberately no SA_RESTART */
	sigaction(SIGUSR1, &sa, NULL);
	sigemptyset(&set);
	sigaddset(&set, SIGUSR1);
	pthread_sigmask(SIG_UNBLOCK, &set, NULL);
}

/* Wait until the thread is sleeping in the kernel (`S` in /proc stat),
 * i.e. it has actually entered the interruptible wait. */
static int last_state;
static int last_stat_errno;

static int wait_thread_sleeping(int tid, int ms)
{
	char path[64], b[256];
	int i;

	snprintf(path, sizeof(path), "/proc/self/task/%d/stat", tid);
	last_state = '?';
	last_stat_errno = 0;
	for (i = 0; i < ms; i++) {
		int fd = open(path, O_RDONLY);
		int n;
		char *p;

		if (fd < 0) {
			last_stat_errno = errno;
		} else {
			n = read(fd, b, sizeof(b) - 1);
			close(fd);
			if (n > 0) {
				b[n] = 0;
				p = strrchr(b, ')');
				if (p && p[1] == ' ') {
					last_state = p[2];
					if (p[2] == 'S')
						return 1;
				}
			}
		}
		usleep(200);
	}
	return 0;
}

static int run_signal_trial(const struct target *t, int delay_us,
			    struct outcome *o)
{
	int rp[2] = { -1, -1 };
	pthread_t th;
	struct p_thread_arg a;
	struct timespec ts;
	int ret = -1, trc, prc, waited;

	memset(o, 0, sizeof(*o));
	memset(&a, 0, sizeof(a));
	o->mode = 'S';
	o->mech = M_SIGNAL;
	o->signal_delay_us = delay_us;
	fn_set(0);
	ff_write("probability", "0");
	if (pipe(rp)) {
		out("HARNESS_ERROR target=%s reason=signal_report_pipe errno=%d\n",
		    t->name, errno);
		return -1;
	}
	if (t->setup(&C, 'S') < 0) {
		out("HARNESS_ERROR target=%s reason=setup phase=S errno=%d\n",
		    t->name, errno);
		goto out;
	}
	install_sig_handler();
	a.t = t;
	if (pthread_create(&th, NULL, p_thread_fn, &a)) {
		out("HARNESS_ERROR target=%s reason=pthread_create\n", t->name);
		goto out;
	}
	{
		int i;

		for (i = 0; i < 100 && !a.started; i++)
			usleep(1000);
	}
	waited = wait_thread_sleeping(a.tid, 200);
	o->waited = waited;
	if (verbose)
		out("SIGWAIT tid=%d waited=%d last_state=%c stat_errno=%d\n",
		    a.tid, waited, last_state, last_stat_errno);
	if (delay_us > 0)
		usleep(delay_us);
	errno = 0;
	prc = pthread_kill(th, SIGUSR1);
	o->signal_rc = prc ? (errno ? errno : prc) : 0;
	clock_gettime(CLOCK_REALTIME, &ts);
	ts.tv_sec += 5;
	trc = pthread_timedjoin_np(th, NULL, &ts);
	if (trc) {
		o->hang = 1;
		app(o->obs, sizeof(o->obs), "p_signal_join_timeout");
		ret = 1;
		goto out;
	}
	o->ret = a.ret;
	o->err = a.err;
	o->injected = t->signal_hit ? t->signal_hit(&C, o) :
		       (o->ret < 0 && o->err == EINTR);
	snprintf(o->site, sizeof(o->site), "sig+%dus", delay_us);
	if (!o->injected) {
		/* The signal arrived after the syscall had already completed;
		 * that is not an injection and is not judged. */
		ret = 0;
		goto out;
	}
	ret = run_survivor(t, rp, o);
out:
	if (rp[0] >= 0)
		close(rp[0]);
	if (rp[1] >= 0)
		close(rp[1]);
	ff_write("probability", "0");
	t->teardown(&C);
	return ret;
}

/*
 * Each trial runs in a fresh child so that the fd table, caches and process
 * state are rebuilt per sample (Dropwise's calibration rule) and a crashing
 * trial cannot take the worker down.  The child reports the whole outcome
 * over one pipe.
 */
struct trial_msg {
	int rc;
	struct outcome o;
};

static int run_trial_isolated(const struct target *t, int mode, unsigned int n,
			      int mech, struct outcome *o)
{
	int rp[2];
	pid_t pid;
	struct pollfd pfd;
	struct trial_msg msg;
	int pr, status, rc = -1;

	if (pipe(rp))
		return -1;
	pid = fork();
	if (pid < 0) {
		close(rp[0]);
		close(rp[1]);
		return -1;
	}
	if (pid == 0) {
		int child_rc;

		close(rp[0]);
		signal(SIGPIPE, SIG_IGN);
		if (open_fail_nth() < 0)
			_exit(2);
		if (t->mechs & (M_FAILNTH | M_FAILFUNC)) {
			struct outcome dry;

			if (run_trial(t, 'D', 0, mech, &dry) != 1)
				out("HARNESS_WARN target=%s dry_failed\n",
				    t->name);
		}
		child_rc = (mode == 'S') ?
			run_signal_trial(t, (int)n, o) :
			run_trial(t, mode, n, mech, o);
		memset(&msg, 0, sizeof(msg));
		msg.rc = child_rc;
		msg.o = *o;
		write_all(rp[1], &msg, sizeof(msg));
		close(rp[1]);
		_exit(0);
	}
	close(rp[1]);
	memset(&msg, 0, sizeof(msg));
	memset(&pfd, 0, sizeof(pfd));
	pfd.fd = rp[0];
	pfd.events = POLLIN;
	pr = poll(&pfd, 1, 30000);
	if (pr > 0) {
		if (read_full(rp[0], &msg, sizeof(msg)) == 0)
			rc = msg.rc;
	}
	close(rp[0]);
	if (pr <= 0)
		kill(pid, SIGKILL);
	waitpid(pid, &status, 0);
	if (pr <= 0) {
		memset(o, 0, sizeof(*o));
		o->hang = 1;
		app(o->obs, sizeof(o->obs), "trial_child_hang=1");
		return 1;
	}
	if (rc < 0 && !WIFEXITED(status)) {
		memset(o, 0, sizeof(*o));
		o->hang = 1;
		app(o->obs, sizeof(o->obs), "trial_child_failed=1");
		return 1;
	}
	*o = msg.o;
	return rc;
}

/* ------------------------------------------------------------------ */
/* judging                                                             */

enum verdict {
	V_UNKNOWN,
	V_OK_PRE,
	V_OK_POST,
	V_OK_PARTIAL,
	V_OK_POISONED,
	V_VIOL_ERR_POST,
	V_VIOL_OK_PRE,
	V_VIOL_MIXED,
	V_VIOL_PARTIAL_POST,
	V_VIOL_PARTIAL_NONE,
};

static const char *vname(enum verdict v)
{
	switch (v) {
	case V_OK_PRE: return "OK_PRE";
	case V_OK_POST: return "OK_POST";
	case V_OK_PARTIAL: return "OK_PARTIAL";
	case V_OK_POISONED: return "OK_POISONED";
	case V_VIOL_ERR_POST: return "VIOL_ERR_POST";
	case V_VIOL_OK_PRE: return "VIOL_OK_PRE";
	case V_VIOL_MIXED: return "VIOL_MIXED";
	case V_VIOL_PARTIAL_POST: return "VIOL_PARTIAL_POST";
	case V_VIOL_PARTIAL_NONE: return "VIOL_PARTIAL_NONE";
	default: return "UNKNOWN";
	}
}

static int is_full(const struct target *t, long ret)
{
	if (t->full == -2)
		return ret >= 0;
	return ret == t->full;
}

static enum verdict judge(const struct target *t, struct outcome *o,
			  const char *pre, const char *post, int stable)
{
	if (!stable)
		return V_UNKNOWN;
	if (o->hang || !o->got_kid)
		return V_UNKNOWN;
	if (o->ret < 0) {
		if (!strcmp(o->obs, pre))
			return V_OK_PRE;
		if (t->error_ok && t->error_ok(&C, o))
			return V_OK_PRE;
		if (t->poisoned_ok && t->poisoned_ok(&C, o))
			return V_OK_POISONED;
		if (!strcmp(o->obs, post))
			return V_VIOL_ERR_POST;
		return V_VIOL_MIXED;
	}
	if (is_full(t, o->ret)) {
		if (!strcmp(o->obs, post))
			return V_OK_POST;
		if (!strcmp(o->obs, pre))
			return V_VIOL_OK_PRE;
		return V_VIOL_MIXED;
	}
	if (o->ret > 0 && o->ret < t->full) {
		if (!strcmp(o->obs, post))
			return V_VIOL_PARTIAL_POST;
		if (!strcmp(o->obs, pre))
			return V_VIOL_PARTIAL_NONE;
		if (t->partial_ok && t->partial_ok(&C, o))
			return V_OK_PARTIAL;
		return V_VIOL_MIXED;
	}
	if (o->ret == 0 && t->full > 0) {
		if (!strcmp(o->obs, pre))
			return V_OK_PRE;
		if (!strcmp(o->obs, post))
			return V_VIOL_PARTIAL_POST;
		return V_VIOL_MIXED;
	}
	return V_UNKNOWN;
}

/* ------------------------------------------------------------------ */
/* one target                                                          */

static unsigned long now_ms(void)
{
	struct timespec ts;

	clock_gettime(CLOCK_MONOTONIC, &ts);
	return (unsigned long)(ts.tv_sec * 1000 + ts.tv_nsec / 1000000);
}

struct mech_stats {
	int sites;
	int trials;
	int misses;
	int ok_pre, ok_post, ok_partial, ok_poisoned;
	int violations, unknown;
	unsigned long started;
};

static void count_verdict(struct mech_stats *st, enum verdict v)
{
	switch (v) {
	case V_OK_PRE: st->ok_pre++; break;
	case V_OK_POST: st->ok_post++; break;
	case V_OK_PARTIAL: st->ok_partial++; break;
	case V_OK_POISONED: st->ok_poisoned++; break;
	case V_UNKNOWN: st->unknown++; break;
	default: st->violations++; break;
	}
}

static const char *mech_name(int mech)
{
	switch (mech) {
	case M_FAILNTH: return "failnth";
	case M_SIGNAL: return "signal";
	case M_FAILFUNC: return "failfunc";
	default: return "?";
	}
}

static void emit_summary(const struct target *t, int mech,
			 const struct mech_stats *st, int stable)
{
	out("SUMMARY target=%s mech=%s sites=%d trials=%d misses=%d ok_pre=%d ok_post=%d ok_partial=%d ok_poisoned=%d violations=%d unknown=%d stable=%d elapsed_ms=%lu\n",
	    t->name, mech_name(mech), st->sites, st->trials, st->misses,
	    st->ok_pre, st->ok_post, st->ok_partial, st->ok_poisoned,
	    st->violations, st->unknown, stable, now_ms() - st->started);
}

static int run_mech_failnth(const struct target *t, const char pre[2][MAX_OBS],
			    const char post[2][MAX_OBS], int stable)
{
	struct mech_stats st;
	struct outcome o;
	int n;

	memset(&st, 0, sizeof(st));
	st.started = now_ms();
	for (n = 1; n <= MAX_N; n++) {
		enum verdict v;
		int rr = run_trial_isolated(t, 'F', (unsigned int)n,
					    M_FAILNTH, &o);

		if (rr < 0) {
			out("TRIAL_END target=%s mech=failnth site=%d reason=harness_error\n",
			    t->name, n);
			break;
		}
		if (rr == 0) {
			out("TRIAL_END target=%s mech=failnth site=%d reason=no_injection readback=%lu ret=%ld errno=%d\n",
			    t->name, n, o.readback, o.ret, o.err);
			break;
		}
		st.sites++;
		st.trials++;
		v = judge(t, &o, pre[0], post[0], stable);
		out("TRIAL target=%s mech=failnth site=%d ret=%ld errno=%d injected=%d readback=%lu verdict=%s obs=%s\n",
		    t->name, n, o.ret, o.err, o.injected, o.readback,
		    vname(v), o.obs);
		count_verdict(&st, v);
		if (o.hang) {
			out("TRIAL_END target=%s mech=failnth site=%d reason=hang\n",
			    t->name, n);
			break;
		}
	}
	emit_summary(t, M_FAILNTH, &st, stable);
	return 0;
}

static int run_mech_failfunc(const struct target *t,
			     const char pre[2][MAX_OBS],
			     const char post[2][MAX_OBS], int stable)
{
	struct mech_stats st;
	int i, rep;

	memset(&st, 0, sizeof(st));
	st.started = now_ms();
	for (i = 0; i < t->nff; i++) {
		const struct ff_site *site = &t->ff[i];
		int hit_any = 0;

		if (ff_register(site) < 0) {
			st.misses++;
			continue;
		}
		for (rep = 0; rep < 2; rep++) {
			struct outcome o;
			enum verdict v;
			int rr = run_trial_isolated(t, 'G', 0, M_FAILFUNC, &o);

			if (rr <= 0) {
				if (rr == 0) {
					out("TRIAL_END target=%s mech=failfunc site=%s reason=not_called readback=%lu\n",
					    t->name, site->func, o.readback);
				} else {
					out("TRIAL_END target=%s mech=failfunc site=%s reason=harness_error\n",
					    t->name, site->func);
				}
				continue;
			}
			hit_any = 1;
			st.trials++;
			v = judge(t, &o, pre[0], post[0], stable);
			out("TRIAL target=%s mech=failfunc site=%s ret=%ld errno=%d injected=%d readback=%lu verdict=%s obs=%s\n",
			    t->name, site->func, o.ret, o.err, o.injected,
			    o.readback, vname(v), o.obs);
			count_verdict(&st, v);
		}
		if (hit_any)
			st.sites++;
		else
			st.misses++;
		ff_unregister(site);
	}
	emit_summary(t, M_FAILFUNC, &st, stable);
	return 0;
}

static int run_mech_signal(const struct target *t,
			   const char pre[2][MAX_OBS],
			   const char post[2][MAX_OBS], int stable)
{
	static const int delays[] = { 0, 200, 2000, 20000 };
	struct mech_stats st;
	int i, rep;

	memset(&st, 0, sizeof(st));
	st.started = now_ms();
	for (i = 0; i < (int)(sizeof(delays) / sizeof(delays[0])); i++) {
		for (rep = 0; rep < 2; rep++) {
			struct outcome o;
			enum verdict v;
			int rr = run_trial_isolated(t, 'S',
						    (unsigned int)delays[i],
						    M_SIGNAL, &o);

			if (rr <= 0) {
				if (rr == 0) {
					st.misses++;
					out("TRIAL_END target=%s mech=signal site=sig+%dus reason=not_interrupted ret=%ld errno=%d waited=%d\n",
					    t->name, delays[i], o.ret, o.err,
					    o.waited);
				} else {
					out("TRIAL_END target=%s mech=signal site=sig+%dus reason=harness_error\n",
					    t->name, delays[i]);
				}
				continue;
			}
			st.sites++;
			st.trials++;
			v = judge(t, &o, pre[0], post[0], stable);
			out("TRIAL target=%s mech=signal site=sig+%dus ret=%ld errno=%d injected=%d waited=%d readback=%d verdict=%s obs=%s\n",
			    t->name, delays[i], o.ret, o.err, o.injected,
			    o.waited, o.signal_rc, vname(v), o.obs);
			count_verdict(&st, v);
		}
	}
	emit_summary(t, M_SIGNAL, &st, stable);
	return 0;
}

static int run_target(const struct target *t)
{
	char pre[2][MAX_OBS], post[2][MAX_OBS];
	struct outcome o;
	int stable = 1;
	int i;

	out("TARGET_BEGIN target=%s full=%ld mechs=%u\n", t->name, t->full,
	    t->mechs);

	if (t->mechs & M_FAILNTH) {
		for (i = 0; i < 3; i++) {
			struct outcome po;
			long calls;

			if (run_trial_isolated(t, 'R', 1000, M_FAILNTH,
					       &po) != 1) {
				out("PROBE_ERROR target=%s sample=%d\n", t->name,
				    i + 1);
				break;
			}
			calls = 1000 - (long)po.readback;
			out("PROBE target=%s sample=%d ret=%ld errno=%d calls=%ld readback=%lu\n",
			    t->name, i + 1, po.ret, po.err, calls, po.readback);
		}
	}

	for (i = 0; i < 2; i++) {
		if (run_trial_isolated(t, 'P', 0, 0, &o) != 1) {
			out("BASELINE_ERROR target=%s side=pre sample=%d reason=run\n",
			    t->name, i + 1);
			return 1;
		}
		if (o.hang) {
			out("BASELINE_HANG target=%s side=pre sample=%d\n",
			    t->name, i + 1);
			return 1;
		}
		snprintf(pre[i], sizeof(pre[i]), "%s", o.obs);
		out("BASE target=%s side=pre sample=%d obs=%s\n", t->name,
		    i + 1, o.obs);
	}
	if (strcmp(pre[0], pre[1])) {
		out("BASELINE_UNSTABLE target=%s side=pre\n", t->name);
		stable = 0;
	}

	for (i = 0; i < 2; i++) {
		if (run_trial_isolated(t, 'O', 0, 0, &o) != 1) {
			out("BASELINE_ERROR target=%s side=post sample=%d reason=run\n",
			    t->name, i + 1);
			return 1;
		}
		if (o.hang) {
			out("BASELINE_HANG target=%s side=post sample=%d\n",
			    t->name, i + 1);
			return 1;
		}
		snprintf(post[i], sizeof(post[i]), "%s", o.obs);
		out("BASE target=%s side=post sample=%d ret=%ld errno=%d obs=%s\n",
		    t->name, i + 1, o.ret, o.err, o.obs);
		if (!is_full(t, o.ret)) {
			out("BASELINE_ERROR target=%s side=post sample=%d ret=%ld errno=%d\n",
			    t->name, i + 1, o.ret, o.err);
			return 1;
		}
	}
	if (strcmp(post[0], post[1])) {
		out("BASELINE_UNSTABLE target=%s side=post\n", t->name);
		stable = 0;
	}
	out("BASE_STABLE target=%s stable=%d pre=%s post=%s\n", t->name,
	    stable, stable ? "same" : "differ", stable ? "same" : "differ");

	if (t->mechs & M_FAILNTH)
		run_mech_failnth(t, (const char (*)[MAX_OBS])pre,
				 (const char (*)[MAX_OBS])post, stable);
	if (t->mechs & M_FAILFUNC)
		run_mech_failfunc(t, (const char (*)[MAX_OBS])pre,
				  (const char (*)[MAX_OBS])post, stable);
	if (t->mechs & M_SIGNAL)
		run_mech_signal(t, (const char (*)[MAX_OBS])pre,
				(const char (*)[MAX_OBS])post, stable);

	out("TARGET_END target=%s\n", t->name);
	return 0;
}

/* ------------------------------------------------------------------ */
/* main                                                                */

static char selected[64] = "all";

static void parse_cmdline(void)
{
	char buf[1024];
	char *p, *save = NULL;
	int n;
	ssize_t r;

	n = open("/proc/cmdline", O_RDONLY);
	if (n < 0)
		return;
	r = read(n, buf, sizeof(buf) - 1);
	close(n);
	if (r <= 0)
		return;
	buf[r] = 0;
	for (p = strtok_r(buf, " ", &save); p; p = strtok_r(NULL, " ", &save)) {
		if (!strncmp(p, "dw.target=", 10))
			snprintf(selected, sizeof(selected), "%s", p + 10);
		else if (!strncmp(p, "dw.verbose=", 11))
			verbose = atoi(p + 11);
		else if (!strncmp(p, "dw.n=", 5))
			trace_n = atoi(p + 5);
	}
}

static pid_t worker_pid;
static volatile sig_atomic_t worker_timed_out;

static void on_alarm(int sig)
{
	(void)sig;
	worker_timed_out = 1;
	if (worker_pid > 0)
		kill(worker_pid, SIGKILL);
}

static void run_trace_one(const struct target *t, int n)
{
	struct outcome o;
	int rr;

	out("TRACE_ONE_BEGIN target=%s n=%d\n", t->name, n);
	if (verbose)
		kmsg_mark("DWTRACE", t->name, (unsigned int)n);
	rr = run_trial_isolated(t, 'F', (unsigned int)n, M_FAILNTH, &o);
	if (rr == 1)
		out("TRACE_ONE target=%s n=%d ret=%ld errno=%d injected=%d readback=%lu obs=%s\n",
		    t->name, n, o.ret, o.err, o.injected, o.readback, o.obs);
	else
		out("TRACE_ONE target=%s n=%d result=no_injection\n",
		    t->name, n);
	out("TARGET_END target=%s\n", t->name);
}

static void run_target_in_worker(const struct target *t)
{
	int status;

	worker_timed_out = 0;
	worker_pid = fork();
	if (worker_pid < 0) {
		out("HARNESS_ERROR target=%s reason=fork_worker errno=%d\n",
		    t->name, errno);
		return;
	}
	if (worker_pid == 0) {
		alarm(0);
		if (open_fail_nth() < 0) {
			out("HARNESS_ERROR target=%s reason=worker_fail_nth\n",
			    t->name);
			_exit(1);
		}
		run_target(t);
		_exit(0);
	}
	signal(SIGALRM, on_alarm);
	alarm(180);
	waitpid(worker_pid, &status, 0);
	alarm(0);
	if (worker_timed_out) {
		out("TARGET_TIMEOUT target=%s\n", t->name);
	} else if (!WIFEXITED(status) || WEXITSTATUS(status) != 0) {
		out("TARGET_WORKER_FAIL target=%s status=%d\n", t->name,
		    status);
	}
	while (waitpid(-1, NULL, WNOHANG) > 0)
		;
}

int main(void)
{
	const char *sel;
	int i, found = 0;

	setvbuf(stdout, NULL, _IONBF, 0);
	signal(SIGPIPE, SIG_IGN);
	mount_all();
	parse_cmdline();
	out("CMDLINE target=%s verbose=%d\n", selected, verbose);
	config_fault_injection();
	net_setup();
	if (ff_config_base() < 0) {
		out("FF_ERROR reason=no_fail_function\n");
	} else {
		static char ff[1048576];
		int n = ff_read_injectable(ff, sizeof(ff));
		char *line, *save = NULL;
		int nonsys = 0;

		out("FF_INJECTABLE entries=%d\n", n);
		for (line = strtok_r(ff, "\n", &save); line;
		     line = strtok_r(NULL, "\n", &save)) {
			if (!strncmp(line, "__x64_sys_", 10) ||
			    !strncmp(line, "__ia32_", 7) ||
			    !strncmp(line, "__do_sys_", 9) ||
			    !strncmp(line, "__do_compat_", 12) ||
			    !strncmp(line, "__x32_", 6))
				continue;
			nonsys++;
			out("FF_LIST %s\n", line);
		}
		out("FF_INJECTABLE nonsyscall=%d\n", nonsys);
	}
	if (ff_config_base() == 0)
		ff_selftest();

	{
		int fd = open("/dev/kmsg", O_WRONLY);

		if (fd >= 0)
			kmsg_fd = fd;
	}

	if (!trace_n && selftest() < 0) {
		out("HARNESS_DONE\n");
		sync();
		reboot(RB_POWER_OFF);
		for (;;)
			pause();
	}

	sel = selected;
	if (trace_n) {
		for (i = 0; all_targets[i]; i++) {
			if (!strcmp(all_targets[i]->name, sel)) {
				run_trace_one(all_targets[i], trace_n);
				found = 1;
				break;
			}
		}
		if (!found)
			out("HARNESS_ERROR reason=unknown_target target=%s\n",
			    sel);
		out("HARNESS_DONE\n");
		sync();
		reboot(RB_POWER_OFF);
		for (;;)
			pause();
	}
	if (!strcmp(sel, "all")) {
		for (i = 0; all_targets[i]; i++)
			run_target_in_worker(all_targets[i]);
	} else {
		for (i = 0; all_targets[i]; i++) {
			if (!strcmp(all_targets[i]->name, sel)) {
				run_target_in_worker(all_targets[i]);
				found = 1;
				break;
			}
		}
		if (!found)
			out("HARNESS_ERROR reason=unknown_target target=%s\n",
			    sel);
	}
	out("HARNESS_DONE\n");
	sync();
	reboot(RB_POWER_OFF);
	for (;;)
		pause();
	return 0;
}
