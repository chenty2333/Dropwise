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
#include <sys/mman.h>
#include <sys/mount.h>
#include <sys/ptrace.h>
#include <sys/reboot.h>
#include <sys/resource.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/user.h>
#include <sys/xattr.h>
#include <netinet/in.h>
#include <netinet/tcp.h>
#include <arpa/inet.h>
#include <net/if.h>
#include <linux/filter.h>
#include <linux/tls.h>
#include <mqueue.h>
#include <sys/prctl.h>
#include <sys/sem.h>
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
	int ws_other_pre;
};

static void epwake_count_sources(struct epwake_ctx *c);

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
	epwake_count_sources(c);
	if (c->ws_other < 0)
		return -1;
	c->ws_other_pre = c->ws_other;
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

static void epwake_count_sources(struct epwake_ctx *c)
{
	char buf[8192], *save = NULL, *line;
	int fd, n, ws_total = 0, ws_ep = 0, ws_other = 0;

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
	if (n <= 0 || n == sizeof(buf) - 1) {
		c->ws_total = c->ws_epoll = c->ws_other = -1;
		return;
	}
	c->ws_total = ws_total;
	c->ws_epoll = ws_ep;
	c->ws_other = ws_other;
}

static void epwake_observer(void *p, const struct kid_report *k,
			    struct outcome *o, char *obs, size_t cap)
{
	struct epwake_ctx *c = p;
	char path[64], buf[8192];
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

	epwake_count_sources(c);
	o->aux[0] = c->tfd_count;
	o->aux[1] = c->tfd_events;
	o->aux[2] = c->ws_total;
	o->aux[3] = c->ws_epoll;
	o->aux[4] = c->ws_other;
	/* Context lives in the isolated child; pass it back with the outcome. */
	o->aux[5] = c->add;
	o->aux[6] = c->ws_other_pre;
	app(obs, cap,
	    "wait_rc=%lld;wait_errno=%d;wait_ev=%lld;wait_data=%lld|tfd=%d;tfd_ev=%x;ws_total=%d;ws_ep=%d;ws_other=%d",
	    k->rc, k->err, k->v[0], k->v[1], c->tfd_count, c->tfd_events,
	    c->ws_total, c->ws_epoll, c->ws_other);
}

static int epwake_error_ok(void *p, struct outcome *o)
{
	(void)p;

	/* ADD and MOD may cache a per-epoll source until ep_free().  Neither
	 * may leave a per-item source on error.  The interest list and the
	 * delivered event/data must still match the operation's pre state. */
	if (o->aux[3] < 0 || o->aux[3] > 1 ||
	    o->aux[4] != o->aux[6] ||
	    o->aux[2] != o->aux[3] + o->aux[4] || o->kid.err != 0)
		return 0;
	if (o->aux[5])
		return o->aux[0] == 0 && o->aux[1] == 0 && o->kid.rc == 0;
	return o->aux[0] == 1 &&
	       o->aux[1] == (EPOLLIN | EPOLLERR | EPOLLHUP) &&
	       o->kid.rc == 1 && o->kid.v[0] == EPOLLIN &&
	       o->kid.v[1] == 0x77;
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
	.error_ok = epwake_error_ok,
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
	.error_ok = epwake_error_ok,
	.teardown = epwake_teardown,
};

/* ------------------------------------------------------------------ */
/* epoll MOD when the per-eventpoll wakeup source already exists       */
/*
 * The first item is installed with EPOLLWAKEUP, so ep->ws exists before P.
 * P modifies a second item; only the per-item wakeup source is missing, and
 * ep_create_wakeup_source() returns -ENOMEM without ever touching ep->ws.
 * An error must therefore leave ep->ws (and the first item's source) alone.
 */
struct epwake_ep_ctx {
	int epfd;
	int p1[2];
	int p2[2];
	int tfd_count;
	int ws_total;
	int ws_ep;
	int ws_other;
	unsigned int tfd2_events;
};

static int epwake_ep_setup(void *p, char phase)
{
	struct epwake_ep_ctx *c = p;
	struct epoll_event ev;

	(void)phase;
	memset(c, 0, sizeof(*c));
	c->epfd = epoll_create1(0);
	if (c->epfd < 0)
		return -1;
	if (pipe2(c->p1, O_NONBLOCK) || pipe2(c->p2, O_NONBLOCK))
		return -1;
	memset(&ev, 0, sizeof(ev));
	ev.events = EPOLLIN | EPOLLWAKEUP;
	ev.data.u64 = 0x11;
	if (epoll_ctl(c->epfd, EPOLL_CTL_ADD, c->p1[0], &ev))
		return -1;
	memset(&ev, 0, sizeof(ev));
	ev.events = EPOLLIN;
	ev.data.u64 = 0x22;
	if (epoll_ctl(c->epfd, EPOLL_CTL_ADD, c->p2[0], &ev))
		return -1;
	return 0;
}

static long epwake_ep_p(void *p, int *err)
{
	struct epwake_ep_ctx *c = p;
	struct epoll_event ev;
	long ret;

	memset(&ev, 0, sizeof(ev));
	ev.events = EPOLLIN | EPOLLWAKEUP;
	ev.data.u64 = 0x1234;
	errno = 0;
	ret = epoll_ctl(c->epfd, EPOLL_CTL_MOD, c->p2[0], &ev);
	*err = ret < 0 ? errno : 0;
	return ret;
}

static void epwake_ep_before_survivor(void *p)
{
	struct epwake_ep_ctx *c = p;

	if (write(c->p2[1], "E", 1) != 1)
		out("HARNESS_WARN target=epwake_ep reason=make_readable errno=%d\n",
		    errno);
}

static void epwake_ep_survivor(void *p, int wfd)
{
	struct epwake_ep_ctx *c = p;
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

static void epwake_ep_observer(void *p, const struct kid_report *k,
			       struct outcome *o, char *obs, size_t cap)
{
	struct epwake_ep_ctx *c = p;
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
		int tfd;
		unsigned int tev;

		count++;
		if (sscanf(s, "tfd: %d events: %x", &tfd, &tev) == 2 &&
		    tfd == c->p2[0])
			ev = tev;
		s += 4;
	}
	c->tfd_count = count;
	c->tfd2_events = ev;

	memset(buf, 0, sizeof(buf));
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
	c->ws_ep = ws_ep;
	c->ws_other = ws_other;
	o->aux[0] = c->tfd_count;
	o->aux[1] = c->tfd2_events;
	o->aux[2] = c->ws_total;
	o->aux[3] = c->ws_ep;
	o->aux[4] = c->ws_other;
	app(obs, cap,
	    "wait_rc=%lld;wait_errno=%d;wait_ev=%lld;wait_data=%lld|tfd=%d;tfd2_ev=%x;ws_total=%d;ws_ep=%d;ws_other=%d",
	    k->rc, k->err, k->v[0], k->v[1], c->tfd_count, c->tfd2_events,
	    c->ws_total, c->ws_ep, c->ws_other);
}

static void epwake_ep_teardown(void *p)
{
	struct epwake_ep_ctx *c = p;

	close(c->p1[0]);
	close(c->p1[1]);
	close(c->p2[0]);
	close(c->p2[1]);
	close(c->epfd);
}

static const struct target t_epwake_ep = {
	.name = "epoll_mod_wakeup_ep",
	.full = 0,
	.mechs = M_FAILNTH,
	.setup = epwake_ep_setup,
	.p = epwake_ep_p,
	.before_survivor = epwake_ep_before_survivor,
	.survivor = epwake_ep_survivor,
	.observer = epwake_ep_observer,
	.teardown = epwake_ep_teardown,
};

/* ================================================================== */
/* round 4: VMA, tmpfs metadata, fd, socket and IPC targets            */
/* ================================================================== */

/* Read a whole (small) proc/sysfs file. */
static int dw_read_file(const char *path, char *buf, size_t cap)
{
	int fd, n, off = 0;

	fd = open(path, O_RDONLY);
	if (fd < 0)
		return -1;
	while (off < (int)cap - 1) {
		n = read(fd, buf + off, cap - 1 - off);
		if (n < 0) {
			if (errno == EINTR)
				continue;
			close(fd);
			return -1;
		}
		if (!n)
			break;
		off += n;
	}
	close(fd);
	buf[off] = 0;
	return off;
}

/* Is addr mapped?  If so also return the first byte.  The caller only uses
 * this on PROT_READ|PROT_WRITE mappings, so the read cannot fault once
 * mincore() has confirmed the VMA is there. */
struct dw_map_state {
	int mapped;
	int err;
	int byte;
};

static void dw_map_probe(unsigned long addr, size_t len, struct dw_map_state *st)
{
	unsigned char vec[2] = { 0, 0 };

	st->mapped = 0;
	st->err = 0;
	st->byte = -1;
	if (!addr || !len)
		return;
	if (mincore((void *)addr, len, vec)) {
		st->err = errno;
		return;
	}
	st->mapped = 1;
	st->byte = *(volatile unsigned char *)addr;
}

/* Protection of the VMA containing addr, from /proc/self/maps ("rw-p"). */
static int dw_maps_perms(unsigned long addr, char *perms, size_t cap)
{
	static char buf[131072];
	char *line, *save = NULL;
	int n;

	perms[0] = 0;
	n = dw_read_file("/proc/self/maps", buf, sizeof(buf));
	if (n <= 0)
		return -1;
	for (line = strtok_r(buf, "\n", &save); line;
	     line = strtok_r(NULL, "\n", &save)) {
		unsigned long s, e;
		char pm[16];

		if (sscanf(line, "%lx-%lx %15s", &s, &e, pm) != 3)
			continue;
		if (addr >= s && addr < e) {
			snprintf(perms, cap, "%s", pm);
			return 0;
		}
	}
	return -1;
}

/* ------------------------------------------------------------------ */
/* target: mmap(MAP_FIXED) replacing a populated private mapping       */
/*
 * Contract: on success the range holds the new zero mapping.  On error the
 * syscall must not silently destroy the caller's existing mapping (the
 * default error-prefix rule); mmap(2) MAP_FIXED says overlapped mappings are
 * discarded, so an error that leaves a gap is reported as contract-external,
 * not as a confirmed bug.  Observer: mincore + content of the address.
 */
struct mmapfix_ctx {
	unsigned long addr;
	size_t len;
	struct dw_map_state obs;
};

static int mmapfix_setup(void *p, char phase)
{
	struct mmapfix_ctx *c = p;
	void *m;

	(void)phase;
	memset(c, 0, sizeof(*c));
	c->len = 4096;
	m = mmap(NULL, c->len, PROT_READ | PROT_WRITE,
		 MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
	if (m == MAP_FAILED)
		return -1;
	c->addr = (unsigned long)m;
	*(volatile unsigned char *)m = 0xA5;
	return 0;
}

static long mmapfix_p(void *p, int *err)
{
	struct mmapfix_ctx *c = p;
	void *r;

	errno = 0;
	r = mmap((void *)c->addr, c->len, PROT_READ | PROT_WRITE,
		 MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED, -1, 0);
	if (r == MAP_FAILED) {
		*err = errno;
		return -1;
	}
	return 0;
}

static void mmapfix_survivor(void *p, int wfd)
{
	struct mmapfix_ctx *c = p;
	struct dw_map_state st;

	dw_map_probe(c->addr, c->len, &st);
	report(wfd, st.mapped, st.err, st.byte, 0, 0, 0, 0);
}

static void mmapfix_observer(void *p, const struct kid_report *k,
			     struct outcome *o, char *obs, size_t cap)
{
	struct mmapfix_ctx *c = p;

	dw_map_probe(c->addr, c->len, &c->obs);
	o->aux[0] = c->obs.mapped;
	o->aux[1] = c->obs.byte;
	app(obs, cap,
	    "surv_mapped=%lld;surv_errno=%d;surv_byte=%lld|obs_mapped=%d;obs_errno=%d;obs_byte=%d",
	    k->rc, k->err, k->v[0], c->obs.mapped, c->obs.err, c->obs.byte);
}

static void mmapfix_teardown(void *p)
{
	struct mmapfix_ctx *c = p;

	if (c->addr)
		munmap((void *)c->addr, c->len);
}

static const struct target t_mmapfix = {
	.name = "mmap_fixed_replace",
	.full = 0,
	.mechs = M_FAILNTH,
	.setup = mmapfix_setup,
	.p = mmapfix_p,
	.survivor = mmapfix_survivor,
	.observer = mmapfix_observer,
	.teardown = mmapfix_teardown,
};

/* ------------------------------------------------------------------ */
/* target: mremap(MREMAP_FIXED) onto an existing populated mapping     */
/*
 * Contract: on success the source is moved to the destination.  On error the
 * source must stay where it was; whether the destination survives is what is
 * measured here (mremap(2) says a previous mapping at new_address "is
 * unmapped", so losing it is contract-external, but it is a concrete
 * counterexample to error-prefix atomicity).  Observer: both addresses.
 */
struct mremap4_ctx {
	unsigned long src, dst;
	size_t len;
	struct dw_map_state s_obs, d_obs;
};

static int mremap4_setup(void *p, char phase)
{
	struct mremap4_ctx *c = p;
	unsigned long base;

	(void)phase;
	memset(c, 0, sizeof(*c));
	c->len = 4096;
	base = (unsigned long)mmap(NULL, 3 * c->len, PROT_READ | PROT_WRITE,
				   MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
	if (base == (unsigned long)MAP_FAILED)
		return -1;
	c->src = base;
	c->dst = base + 2 * c->len;
	*(volatile unsigned char *)c->src = 0x5A;
	*(volatile unsigned char *)c->dst = 0xC3;
	if (mprotect((void *)(base + c->len), c->len, PROT_NONE))
		return -1;
	return 0;
}

static long mremap4_p(void *p, int *err)
{
	struct mremap4_ctx *c = p;
	void *r;

	errno = 0;
	r = mremap((void *)c->src, c->len, c->len,
		   MREMAP_MAYMOVE | MREMAP_FIXED, (void *)c->dst);
	if (r == MAP_FAILED) {
		*err = errno;
		return -1;
	}
	return 0;
}

static void mremap4_survivor(void *p, int wfd)
{
	struct mremap4_ctx *c = p;
	struct dw_map_state s, d;

	dw_map_probe(c->src, c->len, &s);
	dw_map_probe(c->dst, c->len, &d);
	report(wfd, s.mapped, s.err, s.byte, d.mapped, d.err, d.byte, 0);
}

static void mremap4_observer(void *p, const struct kid_report *k,
			     struct outcome *o, char *obs, size_t cap)
{
	struct mremap4_ctx *c = p;

	dw_map_probe(c->src, c->len, &c->s_obs);
	dw_map_probe(c->dst, c->len, &c->d_obs);
	o->aux[0] = c->s_obs.mapped;
	o->aux[1] = c->s_obs.byte;
	o->aux[2] = c->d_obs.mapped;
	o->aux[3] = c->d_obs.byte;
	app(obs, cap,
	    "surv_src=%lld:%lld;dst=%lld:%lld|obs_src=%d:%d;dst=%d:%d",
	    k->rc, k->v[0], k->v[1], k->v[3],
	    c->s_obs.mapped, c->s_obs.byte, c->d_obs.mapped, c->d_obs.byte);
}

static void mremap4_teardown(void *p)
{
	struct mremap4_ctx *c = p;

	if (c->src)
		munmap((void *)c->src, c->len);
	if (c->dst)
		munmap((void *)c->dst, c->len);
}

static const struct target t_mremap4 = {
	.name = "mremap_fixed_dst",
	.full = 0,
	.mechs = M_FAILNTH,
	.setup = mremap4_setup,
	.p = mremap4_p,
	.survivor = mremap4_survivor,
	.observer = mremap4_observer,
	.teardown = mremap4_teardown,
};

/* ------------------------------------------------------------------ */
/* target: mprotect() across three VMAs                                */
/*
 * Three adjacent VMAs with different flags (private, shared, private), so a
 * single mprotect() must change all three.  Failure on the third VMA (split
 * allocation) leaves the first two changed: mprotect(2) documents ENOMEM for
 * "internal kernel structures", and a maintainer states this partial update
 * is not a bug, so a partial is contract-external.  Observer:
 * /proc/self/maps protection of one page in each VMA.
 */
struct mprot4_ctx {
	unsigned long base;
	size_t len;
	char p0[16], p1[16], p2[16];
};

static int mprot4_setup(void *p, char phase)
{
	struct mprot4_ctx *c = p;
	void *m;

	(void)phase;
	memset(c, 0, sizeof(*c));
	c->len = 4096;
	c->base = (unsigned long)mmap(NULL, 5 * c->len,
				      PROT_READ | PROT_WRITE,
				      MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
	if (c->base == (unsigned long)MAP_FAILED)
		return -1;
	*(volatile unsigned char *)c->base = 0x11;
	*(volatile unsigned char *)(c->base + c->len) = 0x22;
	*(volatile unsigned char *)(c->base + 2 * c->len) = 0x33;
	m = mmap((void *)(c->base + c->len), c->len, PROT_READ | PROT_WRITE,
		 MAP_SHARED | MAP_ANONYMOUS | MAP_FIXED, -1, 0);
	if (m == MAP_FAILED)
		return -1;
	return 0;
}

static long mprot4_p(void *p, int *err)
{
	struct mprot4_ctx *c = p;
	long r;

	errno = 0;
	r = mprotect((void *)c->base, 3 * c->len, PROT_READ);
	if (r < 0) {
		*err = errno;
		return -1;
	}
	return 0;
}

static void mprot4_survivor(void *p, int wfd)
{
	struct mprot4_ctx *c = p;

	(void)c;
	report(wfd, 0, 0, 0, 0, 0, 0, 0);
}

static void mprot4_observer(void *p, const struct kid_report *k,
			    struct outcome *o, char *obs, size_t cap)
{
	struct mprot4_ctx *c = p;

	(void)k;
	dw_maps_perms(c->base, c->p0, sizeof(c->p0));
	dw_maps_perms(c->base + c->len, c->p1, sizeof(c->p1));
	dw_maps_perms(c->base + 2 * c->len, c->p2, sizeof(c->p2));
	o->aux[0] = c->p0[0];
	o->aux[1] = c->p1[0];
	o->aux[2] = c->p2[0];
	app(obs, cap, "p0=%s;p1=%s;p2=%s", c->p0, c->p1, c->p2);
}

static void mprot4_teardown(void *p)
{
	struct mprot4_ctx *c = p;

	if (c->base)
		munmap((void *)c->base, 5 * c->len);
}

static const struct target t_mprot4 = {
	.name = "mprotect_multi",
	.full = 0,
	.mechs = M_FAILNTH,
	.setup = mprot4_setup,
	.p = mprot4_p,
	.survivor = mprot4_survivor,
	.observer = mprot4_observer,
	.teardown = mprot4_teardown,
};

/* ------------------------------------------------------------------ */
/* target: tmpfs setxattr replacing an existing value                  */
/*
 * Contract: the new value replaces the old one on success; on error the old
 * value must survive.  Observer: a second independent open+getxattr.
 */
struct xattr4_ctx {
	int fd;
	char path[64];
};

static int xattr4_setup(void *p, char phase)
{
	struct xattr4_ctx *c = p;

	(void)phase;
	memset(c, 0, sizeof(*c));
	c->fd = -1;
	snprintf(c->path, sizeof(c->path), "/tmp/dw_r4_xattr");
	unlink(c->path);
	c->fd = open(c->path, O_RDWR | O_CREAT | O_TRUNC, 0600);
	if (c->fd < 0)
		return -1;
	if (write(c->fd, "hello", 5) != 5)
		return -1;
	if (fsetxattr(c->fd, "user.dw", "AAAA", 4, 0))
		return -1;
	return 0;
}

static long xattr4_p(void *p, int *err)
{
	struct xattr4_ctx *c = p;
	long r;

	errno = 0;
	r = fsetxattr(c->fd, "user.dw", "BBBBBBBB", 8, 0);
	if (r < 0) {
		*err = errno;
		return -1;
	}
	return 0;
}

static void xattr4_survivor(void *p, int wfd)
{
	struct xattr4_ctx *c = p;
	char buf[32];
	ssize_t n;

	memset(buf, 0, sizeof(buf));
	errno = 0;
	n = fgetxattr(c->fd, "user.dw", buf, sizeof(buf));
	report(wfd, n, n < 0 ? errno : 0, n > 0 ? (unsigned char)buf[0] : -1,
	       n > 0 ? (unsigned char)buf[n - 1] : -1, 0, 0, 0);
}

static void xattr4_observer(void *p, const struct kid_report *k,
			    struct outcome *o, char *obs, size_t cap)
{
	struct xattr4_ctx *c = p;
	char buf[32];
	ssize_t n;
	int fd, err = 0;

	memset(buf, 0, sizeof(buf));
	fd = open(c->path, O_RDONLY);
	errno = 0;
	n = fd < 0 ? -1 : fgetxattr(fd, "user.dw", buf, sizeof(buf));
	if (n < 0)
		err = errno;
	if (fd >= 0)
		close(fd);
	o->aux[0] = n;
	app(obs, cap, "surv_len=%lld;surv_b0=%lld;surv_bl=%lld|obs_len=%zd;obs_b0=%d;obs_errno=%d",
	    k->rc, k->v[0], k->v[1], n,
	    n > 0 ? (unsigned char)buf[0] : -1, err);
}

static void xattr4_teardown(void *p)
{
	struct xattr4_ctx *c = p;

	if (c->fd >= 0)
		close(c->fd);
	unlink(c->path);
}

static const struct target t_xattr4 = {
	.name = "tmpfs_setxattr_replace",
	.full = 0,
	.mechs = M_FAILNTH,
	.setup = xattr4_setup,
	.p = xattr4_p,
	.survivor = xattr4_survivor,
	.observer = xattr4_observer,
	.teardown = xattr4_teardown,
};

/* ------------------------------------------------------------------ */
/* target: tmpfs renameat2(RENAME_EXCHANGE)                            */
/*
 * Contract: on success the two names exchange contents; on error both names
 * and contents must survive.  Observer: independent open+read of both names.
 */
struct rn4_ctx {
	char a[64], b[64];
};

static int rn4_read(const char *path, char *buf, int cap, int *err)
{
	int fd, n;

	memset(buf, 0, cap);
	fd = open(path, O_RDONLY);
	if (fd < 0) {
		*err = errno;
		return -1;
	}
	n = read(fd, buf, cap);
	*err = n < 0 ? errno : 0;
	close(fd);
	return n;
}

static int rn4_setup(void *p, char phase)
{
	struct rn4_ctx *c = p;
	int fd;

	(void)phase;
	memset(c, 0, sizeof(*c));
	snprintf(c->a, sizeof(c->a), "/tmp/dw_r4_a");
	snprintf(c->b, sizeof(c->b), "/tmp/dw_r4_b");
	unlink(c->a);
	unlink(c->b);
	fd = open(c->a, O_RDWR | O_CREAT | O_TRUNC, 0600);
	if (fd < 0)
		return -1;
	if (write(fd, "one", 3) != 3) {
		close(fd);
		return -1;
	}
	close(fd);
	fd = open(c->b, O_RDWR | O_CREAT | O_TRUNC, 0600);
	if (fd < 0)
		return -1;
	if (write(fd, "two", 3) != 3) {
		close(fd);
		return -1;
	}
	close(fd);
	return 0;
}

static long rn4_p(void *p, int *err)
{
	struct rn4_ctx *c = p;
	long r;

	errno = 0;
	r = syscall(SYS_renameat2, AT_FDCWD, c->a, AT_FDCWD, c->b,
		    RENAME_EXCHANGE);
	if (r < 0) {
		*err = errno;
		return -1;
	}
	return 0;
}

static void rn4_survivor(void *p, int wfd)
{
	struct rn4_ctx *c = p;
	char ba[8], bb[8];
	int ea = 0, eb = 0, na, nb;

	na = rn4_read(c->a, ba, 4, &ea);
	nb = rn4_read(c->b, bb, 4, &eb);
	report(wfd, na, ea, ba[0], nb, eb, bb[0], 0);
}

static void rn4_observer(void *p, const struct kid_report *k,
			 struct outcome *o, char *obs, size_t cap)
{
	struct rn4_ctx *c = p;
	char ba[8], bb[8];
	int ea = 0, eb = 0, na, nb;

	na = rn4_read(c->a, ba, 4, &ea);
	nb = rn4_read(c->b, bb, 4, &eb);
	o->aux[0] = na;
	o->aux[1] = nb;
	app(obs, cap, "surv_a=%lld:%lld;b=%lld:%lld|obs_a=%d:%d:%d;b=%d:%d:%d",
	    k->rc, k->v[0], k->v[2], k->v[3], na, ea,
	    na > 0 ? (unsigned char)ba[0] : -1, nb, eb,
	    nb > 0 ? (unsigned char)bb[0] : -1);
}

static void rn4_teardown(void *p)
{
	struct rn4_ctx *c = p;

	unlink(c->a);
	unlink(c->b);
}

static const struct target t_rn4 = {
	.name = "tmpfs_rename_exchange",
	.full = 0,
	.mechs = M_FAILNTH,
	.setup = rn4_setup,
	.p = rn4_p,
	.survivor = rn4_survivor,
	.observer = rn4_observer,
	.teardown = rn4_teardown,
};

/* ------------------------------------------------------------------ */
/* target: tmpfs fallocate() extending i_size by populating pages      */
/*
 * Contract: success preallocates the range and extends the size; on error the
 * size must stay unchanged.  Shmem has an explicit undo path; this target
 * checks it.  Observer: fstat size/blocks plus a byte at the end.
 */
struct fall4_ctx {
	int fd;
	char path[64];
};

static int fall4_setup(void *p, char phase)
{
	struct fall4_ctx *c = p;

	(void)phase;
	memset(c, 0, sizeof(*c));
	c->fd = -1;
	snprintf(c->path, sizeof(c->path), "/tmp/dw_r4_falloc");
	unlink(c->path);
	c->fd = open(c->path, O_RDWR | O_CREAT | O_TRUNC, 0600);
	if (c->fd < 0)
		return -1;
	return 0;
}

static long fall4_p(void *p, int *err)
{
	struct fall4_ctx *c = p;
	long r;

	errno = 0;
	r = fallocate(c->fd, 0, 0, 65536);
	if (r < 0) {
		*err = errno;
		return -1;
	}
	return 0;
}

static void fall4_survivor(void *p, int wfd)
{
	struct fall4_ctx *c = p;
	long rc;

	errno = 0;
	rc = pwrite(c->fd, "S", 1, 0);
	report(wfd, rc, rc < 0 ? errno : 0, 0, 0, 0, 0, 0);
}

static void fall4_observer(void *p, const struct kid_report *k,
			   struct outcome *o, char *obs, size_t cap)
{
	struct fall4_ctx *c = p;
	struct stat st;
	char b0 = 0, be = 0;
	ssize_t r0, re;
	int fd;

	memset(&st, 0, sizeof(st));
	if (fstat(c->fd, &st))
		return;
	b0 = be = 0;
	r0 = pread(c->fd, &b0, 1, 0);
	re = pread(c->fd, &be, 1, 65535);
	fd = open(c->path, O_RDONLY);
	o->aux[0] = st.st_size;
	app(obs, cap,
	    "surv_w=%lld;surv_errno=%d|size=%lld;blocks=%lld;b0=%d;bend=%d;r0=%zd;rend=%zd;obs_fd=%d",
	    k->rc, k->err, (long long)st.st_size, (long long)st.st_blocks,
	    (unsigned char)b0, (unsigned char)be, r0, re, fd >= 0);
	if (fd >= 0)
		close(fd);
}

static void fall4_teardown(void *p)
{
	struct fall4_ctx *c = p;

	if (c->fd >= 0)
		close(c->fd);
	unlink(c->path);
}

static const struct target t_fall4 = {
	.name = "tmpfs_fallocate",
	.full = 0,
	.mechs = M_FAILNTH,
	.setup = fall4_setup,
	.p = fall4_p,
	.survivor = fall4_survivor,
	.observer = fall4_observer,
	.teardown = fall4_teardown,
};

/* ------------------------------------------------------------------ */
/* target: fcntl(F_SETPIPE_SZ) replacing the pipe ring                 */
/*
 * Contract: success installs the new capacity and keeps the queued bytes; on
 * error the old capacity and the bytes must survive.  Observer: F_GETPIPE_SZ,
 * FIONREAD and the byte counts after draining.
 */
struct psz4_ctx {
	int pfd[2];
};

static int psz4_setup(void *p, char phase)
{
	struct psz4_ctx *c = p;
	char buf[4096];

	(void)phase;
	memset(c, 0, sizeof(*c));
	c->pfd[0] = c->pfd[1] = -1;
	if (pipe(c->pfd))
		return -1;
	if (fcntl(c->pfd[1], F_SETPIPE_SZ, 65536) < 0)
		return -1;
	memset(buf, 'P', sizeof(buf));
	if (write(c->pfd[1], buf, sizeof(buf)) != (ssize_t)sizeof(buf))
		return -1;
	return 0;
}

static long psz4_p(void *p, int *err)
{
	struct psz4_ctx *c = p;
	long r;

	errno = 0;
	r = fcntl(c->pfd[1], F_SETPIPE_SZ, 131072);
	if (r < 0) {
		*err = errno;
		return -1;
	}
	return 0;
}

static void psz4_survivor(void *p, int wfd)
{
	struct psz4_ctx *c = p;
	long r;

	errno = 0;
	r = write(c->pfd[1], "S", 1);
	report(wfd, r, r < 0 ? errno : 0, 0, 0, 0, 0, 0);
}

static void psz4_observer(void *p, const struct kid_report *k,
			  struct outcome *o, char *obs, size_t cap)
{
	struct psz4_ctx *c = p;
	char buf[16384];
	int fion = 0, np = 0, ns = 0, i;
	long sz;
	ssize_t n;

	sz = fcntl(c->pfd[0], F_GETPIPE_SZ);
	ioctl(c->pfd[0], FIONREAD, &fion);
	n = read(c->pfd[0], buf, sizeof(buf));
	for (i = 0; i < n; i++) {
		if (buf[i] == 'P')
			np++;
		else if (buf[i] == 'S')
			ns++;
	}
	o->aux[0] = sz;
	o->aux[1] = np;
	o->aux[2] = ns;
	app(obs, cap, "surv_w=%lld;surv_errno=%d|size=%ld;fion=%d;np=%d;ns=%d;read=%zd",
	    k->rc, k->err, sz, fion, np, ns, n);
}

static void psz4_teardown(void *p)
{
	struct psz4_ctx *c = p;

	if (c->pfd[0] >= 0)
		close(c->pfd[0]);
	if (c->pfd[1] >= 0)
		close(c->pfd[1]);
}

static const struct target t_psz4 = {
	.name = "pipe_set_size",
	.full = 0,
	.mechs = M_FAILNTH,
	.setup = psz4_setup,
	.p = psz4_p,
	.survivor = psz4_survivor,
	.observer = psz4_observer,
	.teardown = psz4_teardown,
};

/* ------------------------------------------------------------------ */
/* target: SO_ATTACH_FILTER replacing a classic BPF socket filter      */
/*
 * Contract: success replaces the filter; on error the old filter must keep
 * deciding delivery.  Observer: peer bytes -- the survivor sends a datagram
 * from the peer and the receiver's filter decides whether it is delivered.
 */
struct bpf4_ctx {
	int sv[2];
};

static int bpf4_setup(void *p, char phase)
{
	struct bpf4_ctx *c = p;
	struct sock_filter drop[] = { BPF_STMT(BPF_RET | BPF_K, 0) };
	struct sock_fprog prog = { .len = 1, .filter = drop };
	char q = 'Q';

	(void)phase;
	memset(c, 0, sizeof(*c));
	c->sv[0] = c->sv[1] = -1;
	if (socketpair(AF_UNIX, SOCK_DGRAM, 0, c->sv))
		return -1;
	if (setsockopt(c->sv[0], SOL_SOCKET, SO_ATTACH_FILTER, &prog,
		       sizeof(prog)))
		return -1;
	/* This datagram is dropped by the pre filter. */
	if (write(c->sv[1], &q, 1) != 1)
		return -1;
	return 0;
}

static long bpf4_p(void *p, int *err)
{
	struct bpf4_ctx *c = p;
	struct sock_filter pass[] = {
		BPF_STMT(BPF_RET | BPF_K, 0xffffffff)
	};
	struct sock_fprog prog = { .len = 1, .filter = pass };
	long r;

	errno = 0;
	r = setsockopt(c->sv[0], SOL_SOCKET, SO_ATTACH_FILTER, &prog,
		       sizeof(prog));
	if (r < 0) {
		*err = errno;
		return -1;
	}
	return 0;
}

static void bpf4_survivor(void *p, int wfd)
{
	struct bpf4_ctx *c = p;
	char b = 0;
	long r;

	errno = 0;
	if (write(c->sv[1], "X", 1) != 1) {
		report(wfd, -1, errno, -1, 0, 0, 0, 0);
		return;
	}
	r = recv(c->sv[0], &b, 1, MSG_DONTWAIT);
	report(wfd, r, r < 0 ? errno : 0, r > 0 ? (unsigned char)b : -1,
	       0, 0, 0, 0);
}

static void bpf4_observer(void *p, const struct kid_report *k,
			  struct outcome *o, char *obs, size_t cap)
{
	struct bpf4_ctx *c = p;
	char b = 0;
	int fion = 0;
	long r2;

	ioctl(c->sv[0], FIONREAD, &fion);
	errno = 0;
	r2 = recv(c->sv[0], &b, 1, MSG_DONTWAIT);
	o->aux[0] = fion;
	o->aux[1] = r2;
	app(obs, cap,
	    "surv_rc=%lld;surv_errno=%d;surv_byte=%lld|fion=%d;read2=%ld;read2_errno=%d",
	    k->rc, k->err, k->v[0], fion, r2, r2 < 0 ? errno : 0);
}

static void bpf4_teardown(void *p)
{
	struct bpf4_ctx *c = p;

	if (c->sv[0] >= 0)
		close(c->sv[0]);
	if (c->sv[1] >= 0)
		close(c->sv[1]);
}

static const struct target t_bpf4 = {
	.name = "so_attach_filter",
	.full = 0,
	.mechs = M_FAILNTH,
	.setup = bpf4_setup,
	.p = bpf4_p,
	.survivor = bpf4_survivor,
	.observer = bpf4_observer,
	.teardown = bpf4_teardown,
};

/* ------------------------------------------------------------------ */
/* target: IP_ADD_MEMBERSHIP                                           */
/*
 * Contract: success joins the group, i.e. the socket's membership exists AND
 * the interface has the group (IGMP report / delivery).  The socket-side list
 * is created before ____ip_mc_inc_group(), whose kzalloc failure is ignored
 * by the void return: success with only the socket-side half must be visible
 * as a violation.  Observer: /proc/net/igmp for the device-side group and
 * IP_MSFILTER for the socket-side membership.
 */
#define DW_MC_GROUP "239.1.2.3"
#define DW_MC_HEX   "030201EF"	/* /proc/net/igmp prints the host-order %08X */

struct mc4_ctx {
	int fd;
	struct ip_mreqn mreq;
};

static int mc4_setup(void *p, char phase)
{
	struct mc4_ctx *c = p;

	(void)phase;
	memset(c, 0, sizeof(*c));
	c->fd = -1;
	c->fd = socket(AF_INET, SOCK_DGRAM, 0);
	if (c->fd < 0)
		return -1;
	inet_pton(AF_INET, DW_MC_GROUP, &c->mreq.imr_multiaddr);
	inet_pton(AF_INET, "127.0.0.1", &c->mreq.imr_address);
	c->mreq.imr_ifindex = 0;
	return 0;
}

static long mc4_p(void *p, int *err)
{
	struct mc4_ctx *c = p;
	long r;

	errno = 0;
	r = setsockopt(c->fd, IPPROTO_IP, IP_ADD_MEMBERSHIP, &c->mreq,
		       sizeof(c->mreq));
	if (r < 0) {
		*err = errno;
		return -1;
	}
	return 0;
}

/* Socket-side membership probe: IP_MSFILTER only succeeds for a group that
 * this socket has joined.  It does not modify any state. */
static long mc4_sock_msf(int fd, int *err)
{
	struct ip_msfilter msf;
	socklen_t len = IP_MSFILTER_SIZE(0);

	memset(&msf, 0, sizeof(msf));
	inet_pton(AF_INET, DW_MC_GROUP, &msf.imsf_multiaddr);
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

static int mc4_igmp_group(void)
{
	static char buf[16384];
	int n;

	n = dw_read_file("/proc/net/igmp", buf, sizeof(buf));
	if (n <= 0)
		return -1;
	return strstr(buf, DW_MC_HEX) != NULL;
}

static void mc4_survivor(void *p, int wfd)
{
	struct mc4_ctx *c = p;
	int err = 0;
	long r;

	r = mc4_sock_msf(c->fd, &err);
	report(wfd, r, err, 0, 0, 0, 0, 0);
}

static void mc4_observer(void *p, const struct kid_report *k,
			 struct outcome *o, char *obs, size_t cap)
{
	struct mc4_ctx *c = p;
	int err = 0, igmp;
	long r;

	igmp = mc4_igmp_group();
	r = mc4_sock_msf(c->fd, &err);
	o->aux[0] = igmp;
	o->aux[1] = r;
	app(obs, cap,
	    "surv_msf=%lld;surv_errno=%d|obs_msf=%ld;obs_errno=%d;igmp_group=%d",
	    k->rc, k->err, r, err, igmp);
}

static void mc4_teardown(void *p)
{
	struct mc4_ctx *c = p;

	if (c->fd >= 0)
		close(c->fd);
}

static const struct target t_mc4 = {
	.name = "ip_add_membership",
	.full = 0,
	.mechs = M_FAILNTH,
	.setup = mc4_setup,
	.p = mc4_p,
	.survivor = mc4_survivor,
	.observer = mc4_observer,
	.teardown = mc4_teardown,
};

/* ------------------------------------------------------------------ */
/* target: semtimedop() with two operations, interrupted               */
/*
 * POSIX atomicity: either all operations are performed or none.  The two-op
 * set is arranged so op0 can run but op1 must block; a signal therefore has
 * to return EINTR with BOTH semaphores unchanged.  Observer: GETVAL.
 */
union dw_semun {
	int val;
	struct semid_ds *buf;
	unsigned short *array;
	struct seminfo *__buf;
};

struct sem4_ctx {
	int semid;
};

static int sem4_setup(void *p, char phase)
{
	struct sem4_ctx *c = p;
	unsigned short vals[2];
	union dw_semun u;

	memset(c, 0, sizeof(*c));
	c->semid = semget(IPC_PRIVATE, 2, IPC_CREAT | 0600);
	if (c->semid < 0)
		return -1;
	vals[0] = 1;
	vals[1] = (phase == 'O') ? 1 : 0;
	memset(&u, 0, sizeof(u));
	u.array = vals;
	if (semctl(c->semid, 0, SETALL, u) < 0)
		return -1;
	return 0;
}

static long sem4_p(void *p, int *err)
{
	struct sem4_ctx *c = p;
	struct sembuf sops[2];
	struct timespec ts;
	long r;

	memset(sops, 0, sizeof(sops));
	sops[0].sem_num = 0;
	sops[0].sem_op = -1;
	sops[1].sem_num = 1;
	sops[1].sem_op = -1;
	ts.tv_sec = 0;
	ts.tv_nsec = 500000000;
	errno = 0;
	r = syscall(SYS_semtimedop, c->semid, sops, 2, &ts);
	if (r < 0) {
		*err = errno;
		return -1;
	}
	return 0;
}

static void sem4_survivor(void *p, int wfd)
{
	struct sem4_ctx *c = p;
	long v0, v1;

	v0 = semctl(c->semid, 0, GETVAL);
	v1 = semctl(c->semid, 1, GETVAL);
	report(wfd, v0, 0, v1, 0, 0, 0, 0);
}

static void sem4_observer(void *p, const struct kid_report *k,
			  struct outcome *o, char *obs, size_t cap)
{
	struct sem4_ctx *c = p;
	long v0, v1;

	v0 = semctl(c->semid, 0, GETVAL);
	v1 = semctl(c->semid, 1, GETVAL);
	o->aux[0] = v0;
	o->aux[1] = v1;
	app(obs, cap, "surv_s0=%lld;surv_s1=%lld|s0=%ld;s1=%ld",
	    k->rc, k->v[0], v0, v1);
}

static void sem4_teardown(void *p)
{
	struct sem4_ctx *c = p;

	if (c->semid >= 0)
		semctl(c->semid, 0, IPC_RMID);
}

static const struct target t_sem4 = {
	.name = "semtimedop_eintr",
	.full = 0,
	.mechs = M_SIGNAL,
	.setup = sem4_setup,
	.p = sem4_p,
	.survivor = sem4_survivor,
	.observer = sem4_observer,
	.teardown = sem4_teardown,
};

/* ------------------------------------------------------------------ */
/* target: fcntl(F_SETLKW) interrupted while waiting for a record lock */
/*
 * Contract: on EINTR the lock was not acquired; the process must look like it
 * never called F_SETLKW.  The pre/post phases differ in whether an external
 * holder exists, which makes "no lock" (pre) and "P holds the lock" (post)
 * distinguishable.  The observer kills the external holder first, then the
 * survivor (a different process, so POSIX locks are not inherited) tries a
 * non-blocking lock: success means P left nothing behind.
 */
struct lkw4_ctx {
	int fd;
	char path[64];
	int holder_pid;
	int holder_killed;
};

static void lkw4_holder_die(int sig)
{
	(void)sig;
	_exit(0);
}

static int lkw4_setup(void *p, char phase)
{
	struct lkw4_ctx *c = p;
	struct flock fl;
	int rp[2];
	pid_t pid;
	char b;

	memset(c, 0, sizeof(*c));
	c->fd = -1;
	c->holder_pid = -1;
	snprintf(c->path, sizeof(c->path), "/tmp/dw_r4_lkw");
	unlink(c->path);
	c->fd = open(c->path, O_RDWR | O_CREAT | O_TRUNC, 0600);
	if (c->fd < 0)
		return -1;
	if (write(c->fd, "01234567", 8) != 8)
		return -1;
	if (phase != 'P' && phase != 'S')
		return 0;
	if (pipe(rp))
		return -1;
	pid = fork();
	if (pid < 0)
		return -1;
	c->holder_pid = pid;
	if (pid == 0) {
		int fd2;

		close(rp[0]);
		signal(SIGUSR1, lkw4_holder_die);
		prctl(PR_SET_PDEATHSIG, SIGKILL);
		fd2 = open(c->path, O_RDWR);
		if (fd2 < 0)
			_exit(1);
		memset(&fl, 0, sizeof(fl));
		fl.l_type = F_WRLCK;
		fl.l_whence = SEEK_SET;
		if (fcntl(fd2, F_SETLK, &fl))
			_exit(1);
		b = 'R';
		if (write(rp[1], &b, 1) != 1)
			_exit(1);
		for (;;)
			pause();
	}
	close(rp[1]);
	b = 0;
	if (read(rp[0], &b, 1) != 1) {
		close(rp[0]);
		return -1;
	}
	close(rp[0]);
	c->holder_pid = pid;
	return 0;
}

static long lkw4_p(void *p, int *err)
{
	struct lkw4_ctx *c = p;
	struct flock fl;
	long r;

	memset(&fl, 0, sizeof(fl));
	fl.l_type = F_WRLCK;
	fl.l_whence = SEEK_SET;
	errno = 0;
	r = fcntl(c->fd, F_SETLKW, &fl);
	if (r < 0) {
		*err = errno;
		return -1;
	}
	return 0;
}

static void lkw4_before_survivor(void *p)
{
	struct lkw4_ctx *c = p;
	int status;

	if (c->holder_pid <= 0)
		return;
	kill(c->holder_pid, SIGKILL);
	while (waitpid(c->holder_pid, &status, 0) < 0 && errno == EINTR)
		;
	c->holder_pid = -1;
	c->holder_killed = 1;
}

static void lkw4_survivor(void *p, int wfd)
{
	struct lkw4_ctx *c = p;
	struct flock fl;
	int fd;
	long r;

	fd = open(c->path, O_RDWR);
	if (fd < 0) {
		report(wfd, -1, errno, 0, 0, 0, 0, 0);
		return;
	}
	memset(&fl, 0, sizeof(fl));
	fl.l_type = F_WRLCK;
	fl.l_whence = SEEK_SET;
	errno = 0;
	r = fcntl(fd, F_SETLK, &fl);
	report(wfd, r, r < 0 ? errno : 0, 0, 0, 0, 0, 0);
	close(fd);
}

static void lkw4_observer(void *p, const struct kid_report *k,
			  struct outcome *o, char *obs, size_t cap)
{
	struct lkw4_ctx *c = p;

	o->aux[0] = c->holder_killed;
	app(obs, cap, "holder_killed=%d;surv_lock_rc=%lld;surv_lock_errno=%d",
	    c->holder_killed, k->rc, k->err);
}

static void lkw4_teardown(void *p)
{
	struct lkw4_ctx *c = p;
	int status;

	if (c->holder_pid > 0) {
		kill(c->holder_pid, SIGKILL);
		while (waitpid(c->holder_pid, &status, 0) < 0 &&
		       errno == EINTR)
			;
		c->holder_pid = -1;
	}
	if (c->fd >= 0)
		close(c->fd);
	unlink(c->path);
}

static const struct target t_lkw4 = {
	.name = "fcntl_setlkw_eintr",
	.full = 0,
	.mechs = M_SIGNAL,
	.setup = lkw4_setup,
	.p = lkw4_p,
	.before_survivor = lkw4_before_survivor,
	.survivor = lkw4_survivor,
	.observer = lkw4_observer,
	.teardown = lkw4_teardown,
};

static const struct target *all_targets[] = {
	&t_scm, &t_pipe, &t_tmpfs, &t_epoll, &t_lock, &t_accept,
	&t_tls_tx, &t_tls_rx, &t_ovl, &t_mq_send,
	&t_pipe_sig_block, &t_pipe_sig_part,
	&t_epwake_mod, &t_epwake_add, &t_epwake_ep,
	&t_mmapfix, &t_mremap4, &t_mprot4,
	&t_xattr4, &t_rn4, &t_fall4, &t_psz4, &t_bpf4, &t_mc4,
	&t_sem4, &t_lkw4, NULL
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
/* round-3 real-trigger probes                                         */
/*
 * These probes try to make the wakeup-source allocation fail without
 * fail-nth/failslab.  They reuse the round-2 observation (epoll fdinfo event
 * mask + /sys/kernel/debug/wakeup_sources) and print PROBE_* lines.  They are
 * not part of the target/verdict machinery and are selected with
 * dw.target=probe_*; main() branches to run_probe() before the selftest.
 */

#define PROBE_CG_ROOT "/sys/fs/cgroup"
#define PROBE_CG_MEM  "/sys/fs/cgroup/dwprobe"
#define PROBE_WS_PATH "/sys/kernel/debug/wakeup_sources"

struct probe_ws {
	int total;
	int ep;
	int other;
	int truncated;
};

struct probe_msg {
	int stage;		/* 1 ready, 2 mod result, -1 error */
	int iter;
	int fd;
	long ret;
	int err;
	unsigned int events;
};

static int probe_read_file(const char *path, char *buf, size_t cap)
{
	int fd;
	ssize_t n, off = 0;

	fd = open(path, O_RDONLY);
	if (fd < 0)
		return -1;
	while (off < (ssize_t)cap - 1) {
		n = read(fd, buf + off, cap - 1 - off);
		if (n < 0) {
			if (errno == EINTR)
				continue;
			close(fd);
			return -1;
		}
		if (n == 0)
			break;
		off += n;
	}
	close(fd);
	buf[off] = 0;
	return (int)off;
}

static long probe_read_long(const char *path)
{
	char b[64];
	char *end;
	long v;

	if (probe_read_file(path, b, sizeof(b)) <= 0)
		return -1;
	errno = 0;
	v = strtol(b, &end, 10);
	if (errno)
		return -1;
	return v;
}

static int probe_write_pid(const char *path, pid_t pid)
{
	char b[32];
	int fd, len;
	ssize_t n;

	len = snprintf(b, sizeof(b), "%d", (int)pid);
	fd = open(path, O_WRONLY);
	if (fd < 0)
		return -1;
	n = write(fd, b, len);
	close(fd);
	return n == len ? 0 : -1;
}

static void probe_set_nofile(unsigned long n)
{
	struct rlimit rl;

	if (getrlimit(RLIMIT_NOFILE, &rl))
		return;
	if (rl.rlim_max < n)
		rl.rlim_max = n;
	rl.rlim_cur = rl.rlim_max;
	setrlimit(RLIMIT_NOFILE, &rl);
}

static void probe_ws_count(struct probe_ws *c)
{
	static char buf[4 * 1024 * 1024];
	char *line, *save = NULL;
	size_t off = 0;
	int fd, n;

	memset(c, 0, sizeof(*c));
	fd = open(PROBE_WS_PATH, O_RDONLY);
	if (fd < 0) {
		c->total = -1;
		return;
	}
	while (off < sizeof(buf) - 1) {
		n = read(fd, buf + off, sizeof(buf) - 1 - off);
		if (n < 0) {
			if (errno == EINTR)
				continue;
			break;
		}
		if (n == 0)
			break;
		off += n;
	}
	close(fd);
	c->truncated = off >= sizeof(buf) - 1;
	buf[off] = 0;
	for (line = strtok_r(buf, "\n", &save); line;
	     line = strtok_r(NULL, "\n", &save)) {
		char *name = line;
		char *sp;

		while (*name == ' ' || *name == '\t')
			name++;
		sp = name;
		while (*sp && *sp != ' ' && *sp != '\t')
			sp++;
		if (sp == name)
			continue;
		if (sp - name == 4 && !strncmp(name, "name", 4))
			continue;
		c->total++;
		if (sp - name == 9 && !strncmp(name, "eventpoll", 9))
			c->ep++;
		else
			c->other++;
	}
}

static unsigned int probe_fdinfo_events(int epfd)
{
	char path[64], buf[4096];
	char *s;
	unsigned int ev = 0;
	int fd, n;

	snprintf(path, sizeof(path), "/proc/self/fdinfo/%d", epfd);
	fd = open(path, O_RDONLY);
	if (fd < 0)
		return 0;
	n = read(fd, buf, sizeof(buf) - 1);
	close(fd);
	if (n <= 0)
		return 0;
	buf[n] = 0;
	s = strstr(buf, "events:");
	if (s)
		sscanf(s + 7, "%x", &ev);
	return ev;
}

static unsigned int probe_pid_fdinfo_events(pid_t pid, int epfd)
{
	char path[64], buf[4096];
	char *s;
	unsigned int ev = 0;
	int fd, n;

	snprintf(path, sizeof(path), "/proc/%d/fdinfo/%d", (int)pid, epfd);
	fd = open(path, O_RDONLY);
	if (fd < 0)
		return 0;
	n = read(fd, buf, sizeof(buf) - 1);
	close(fd);
	if (n <= 0)
		return 0;
	buf[n] = 0;
	s = strstr(buf, "events:");
	if (s)
		sscanf(s + 7, "%x", &ev);
	return ev;
}

static int probe_epoll_setup(int *epfd, int pfd[2], int add_wakeup)
{
	struct epoll_event ev;

	*epfd = epoll_create1(0);
	if (*epfd < 0)
		return -1;
	if (pipe2(pfd, O_NONBLOCK))
		return -1;
	memset(&ev, 0, sizeof(ev));
	ev.events = EPOLLIN | (add_wakeup ? EPOLLWAKEUP : 0);
	ev.data.u64 = 0x77;
	if (epoll_ctl(*epfd, EPOLL_CTL_ADD, pfd[0], &ev))
		return -1;
	return 0;
}

static int probe_read_msg_timeout(int fd, struct probe_msg *m, int ms)
{
	struct pollfd pfd;

	memset(m, 0, sizeof(*m));
	memset(&pfd, 0, sizeof(pfd));
	pfd.fd = fd;
	pfd.events = POLLIN;
	if (poll(&pfd, 1, ms) <= 0)
		return -1;
	return read_full(fd, m, sizeof(*m));
}

static int probe_wait_timeout(pid_t pid, int ms, int *status)
{
	int i, r;

	for (i = 0; i < ms / 10; i++) {
		r = waitpid(pid, status, WNOHANG);
		if (r == pid)
			return 0;
		if (r < 0)
			return -1;
		usleep(10000);
	}
	kill(pid, SIGKILL);
	waitpid(pid, status, 0);
	return 1;
}

static long probe_read_oom_kill(void)
{
	char b[4096];
	char *s;

	if (probe_read_file(PROBE_CG_MEM "/memory.events", b, sizeof(b)) <= 0)
		return -1;
	s = strstr(b, "oom_kill ");
	if (!s)
		return -1;
	return strtol(s + 9, NULL, 10);
}

static int probe_mount_cgroup(void)
{
	char b[256];

	mkdir_if_needed(PROBE_CG_ROOT, 0755);
	if (mount("cgroup2", PROBE_CG_ROOT, "cgroup2", 0, "")) {
		if (errno != EBUSY) {
			out("PROBE_CG mount=fail errno=%d\n", errno);
			return -1;
		}
	}
	if (probe_read_file(PROBE_CG_ROOT "/cgroup.controllers", b,
			    sizeof(b)) <= 0 || !strstr(b, "memory")) {
		out("PROBE_CG memory_controller=absent controllers=%s\n",
		    b[0] ? b : "(none)");
		return -1;
	}
	if (write_str(PROBE_CG_ROOT "/cgroup.subtree_control", "+memory"))
		out("PROBE_CG subtree_memory=fail errno=%d\n", errno);
	if (mkdir_if_needed(PROBE_CG_MEM, 0755))
		return -1;
	if (write_str(PROBE_CG_MEM "/memory.max", "64M"))
		out("PROBE_CG memory_max_init=fail errno=%d\n", errno);
	write_str(PROBE_CG_MEM "/memory.swap.max", "0");
	out("PROBE_CG mounted=1 memory_controller=1\n");
	return 0;
}

static int probe_oom_protect = 1;

static int probe_memcg_one(int trial, const struct probe_ws *base,
			   int *viol, int *ret_err, int *child_killed)
{
	int go[2], res[2], status;
	pid_t pid;
	char ch = 'x';
	struct probe_msg m;
	struct probe_ws ws;
	long cur, maxv;
	char mbuf[32];
	unsigned int ev = 0;
	int rc = -1;

	*viol = 0;
	if (write_str(PROBE_CG_MEM "/memory.max", "64M")) {
		out("PROBE_MEMCG trial=%d max_reset=fail errno=%d\n", trial, errno);
		return -1;
	}
	if (pipe(go) || pipe(res))
		return -1;
	/* Pre-fault both pipe pages so the sync writes below are allocation
	 * free once memory.max is tightened. */
	if (write(go[1], &ch, 1) != 1 || read(go[0], &ch, 1) != 1)
		return -1;
	if (write(res[1], &ch, 1) != 1 || read(res[0], &ch, 1) != 1)
		return -1;

	pid = fork();
	if (pid == 0) {
		int epfd, pfd[2];
		void *p;
		size_t chunk = 32UL << 20;
		struct epoll_event ev;

		close(go[1]);
		close(res[0]);
		/*
		 * Keep the allocating task out of the memcg OOM victim
		 * selection.  A charge failure then has to return -ENOMEM
		 * instead of killing this child, which is what makes a
		 * memcg-triggered failure observable as a syscall return.
		 */
		if (probe_oom_protect &&
		    write_str("/proc/self/oom_score_adj", "-1000"))
			out("PROBE_MEMCG trial=%d oom_score_adj=fail errno=%d\n",
			    trial, errno);
		if (read(go[0], &ch, 1) != 1)
			_exit(2);
		if (probe_epoll_setup(&epfd, pfd, 0)) {
			memset(&m, 0, sizeof(m));
			m.stage = -1;
			m.iter = trial;
			m.err = errno;
			write(res[1], &m, sizeof(m));
			_exit(3);
		}
		p = mmap(NULL, chunk, PROT_READ | PROT_WRITE,
			 MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
		if (p == MAP_FAILED) {
			memset(&m, 0, sizeof(m));
			m.stage = -1;
			m.iter = trial;
			m.err = errno;
			write(res[1], &m, sizeof(m));
			_exit(4);
		}
		memset(p, 0x41, chunk);
		memset(&m, 0, sizeof(m));
		m.stage = 1;
		m.iter = trial;
		m.fd = epfd;
		write(res[1], &m, sizeof(m));
		if (read(go[0], &ch, 1) != 1)
			_exit(5);

		memset(&ev, 0, sizeof(ev));
		ev.events = EPOLLIN | EPOLLWAKEUP;
		ev.data.u64 = 0x1234;
		errno = 0;
		memset(&m, 0, sizeof(m));
		m.stage = 2;
		m.iter = trial;
		m.fd = epfd;
		m.events = ev.events;
		m.ret = epoll_ctl(epfd, EPOLL_CTL_MOD, pfd[0], &ev);
		m.err = errno;
		write(res[1], &m, sizeof(m));
		if (read(go[0], &ch, 1) != 1)
			_exit(6);
		_exit(0);
	}

	close(go[0]);
	close(res[1]);
	if (pid < 0) {
		close(go[1]);
		close(res[0]);
		return -1;
	}
	if (probe_write_pid(PROBE_CG_MEM "/cgroup.procs", pid)) {
		out("PROBE_MEMCG trial=%d move_child=fail errno=%d\n", trial,
		    errno);
		goto kill_child;
	}
	if (write(go[1], &ch, 1) != 1)
		goto kill_child;
	if (probe_read_msg_timeout(res[0], &m, 5000) || m.stage != 1) {
		out("PROBE_MEMCG trial=%d setup=fail stage=%d errno=%d\n",
		    trial, m.stage, m.err);
		goto kill_child;
	}

	cur = probe_read_long(PROBE_CG_MEM "/memory.current");
	if (cur < 0)
		cur = 0;
	/*
	 * Put memory.max far below the current usage so the cgroup has no
	 * headroom at all.  Unaccounted allocations still succeed; any
	 * accounted allocation on the path must fail.
	 */
	maxv = 1024 * 1024;
	snprintf(mbuf, sizeof(mbuf), "%ld", maxv);
	if (write_str(PROBE_CG_MEM "/memory.max", mbuf))
		out("PROBE_MEMCG trial=%d max_tight=fail errno=%d\n", trial,
		    errno);
	if (write(go[1], &ch, 1) != 1)
		goto kill_child;

	if (probe_read_msg_timeout(res[0], &m, 5000) || m.stage != 2) {
		int st;

		*child_killed = 1;
		if (probe_wait_timeout(pid, 1000, &st) == 0 &&
		    WIFSIGNALED(st))
			out("PROBE_MEMCG trial=%d child_killed_by=%d max=%ld cur=%ld\n",
			    trial, WTERMSIG(st), maxv, cur);
		else
			out("PROBE_MEMCG trial=%d mod_no_result max=%ld cur=%ld\n",
			    trial, maxv, cur);
		rc = -2;
		goto count_and_exit;
	}

	probe_ws_count(&ws);
	ev = probe_pid_fdinfo_events(pid, m.fd);
	if (m.ret == 0) {
		int ok = ws.ep == base->ep + 1 && ws.other == base->other + 1;

		if (!ok) {
			*viol = 1;
			out("PROBE_MEMCG trial=%d VERDICT=VIOL ret=0 errno=%d"
			    " base_ep=%d base_other=%d ep=%d other=%d"
			    " fdinfo_events=%x cur_before=%ld max=%ld\n",
			    trial, m.err, base->ep, base->other, ws.ep,
			    ws.other, ev, cur, maxv);
		} else {
			out("PROBE_MEMCG trial=%d ret=0 errno=%d ep=%d other=%d"
			    " fdinfo_events=%x cur_before=%ld max=%ld"
			    " sources=ok\n",
			    trial, m.err, ws.ep, ws.other, ev, cur, maxv);
		}
	} else {
		(*ret_err)++;
		out("PROBE_MEMCG trial=%d ret=%ld errno=%d ep=%d other=%d"
		    " fdinfo_events=%x cur_before=%ld max=%ld\n",
		    trial, m.ret, m.err, ws.ep, ws.other, ev, cur, maxv);
	}
	rc = 0;

count_and_exit:
	write(go[1], &ch, 1);
	if (probe_wait_timeout(pid, 5000, &status) != 0)
		out("PROBE_MEMCG trial=%d child_wait_timeout\n", trial);
	close(go[1]);
	close(res[0]);
	return rc;

kill_child:
	kill(pid, SIGKILL);
	waitpid(pid, &status, 0);
	close(go[1]);
	close(res[0]);
	return -1;
}

static void probe_memcg_pressure(void)
{
	int go[2], status;
	pid_t pid;
	char ch = 'x';
	long before, after;

	write_str(PROBE_CG_MEM "/memory.max", "1M");
	before = probe_read_oom_kill();
	if (pipe(go))
		return;
	pid = fork();
	if (pid == 0) {
		void *p;

		close(go[1]);
		if (read(go[0], &ch, 1) != 1)
			_exit(2);
		p = mmap(NULL, 4UL << 20, PROT_READ | PROT_WRITE,
			 MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
		if (p == MAP_FAILED)
			_exit(3);
		memset(p, 0x42, 4UL << 20);
		_exit(0);
	}
	close(go[0]);
	if (pid < 0) {
		close(go[1]);
		return;
	}
	if (probe_write_pid(PROBE_CG_MEM "/cgroup.procs", pid))
		out("PROBE_MEMCG pressure_move=fail errno=%d\n", errno);
	write(go[1], &ch, 1);
	if (probe_wait_timeout(pid, 5000, &status) == 0) {
		after = probe_read_oom_kill();
		if (WIFSIGNALED(status))
			out("PROBE_MEMCG pressure killed_by=%d oom_kill=%ld->%ld\n",
			    WTERMSIG(status), before, after);
		else
			out("PROBE_MEMCG pressure survived exit=%d oom_kill=%ld->%ld\n",
			    WIFEXITED(status) ? WEXITSTATUS(status) : -1,
			    before, after);
	} else {
		out("PROBE_MEMCG pressure wait_timeout\n");
	}
	close(go[1]);
}

static int probe_selinux_load(void)
{
	const char *ctx = "system_u:object_r:sysfs_t:s0";
	char b[256];
	struct stat st;
	char *buf = NULL;
	int pfd, fd, r;
	ssize_t n;

	mkdir_if_needed("/sys/fs/selinux", 0755);
	if (mount("selinuxfs", "/sys/fs/selinux", "selinuxfs", 0, "")) {
		out("PROBE_SELINUX mount=fail errno=%d\n", errno);
		return -1;
	}
	pfd = open("/policy.bin", O_RDONLY);
	if (pfd < 0 || fstat(pfd, &st)) {
		out("PROBE_SELINUX policy_file=fail errno=%d\n", errno);
		if (pfd >= 0)
			close(pfd);
		return -1;
	}
	buf = malloc(st.st_size);
	if (!buf) {
		out("PROBE_SELINUX policy_malloc=fail size=%lld\n",
		    (long long)st.st_size);
		close(pfd);
		return -1;
	}
	for (n = 0; n < st.st_size; ) {
		r = read(pfd, buf + n, st.st_size - n);
		if (r <= 0)
			break;
		n += r;
	}
	close(pfd);
	fd = open("/sys/fs/selinux/load", O_WRONLY);
	if (fd < 0) {
		out("PROBE_SELINUX load_open=fail errno=%d\n", errno);
		free(buf);
		return -1;
	}
	errno = 0;
	r = write(fd, buf, st.st_size);
	out("PROBE_SELINUX load size=%lld written=%d errno=%d\n",
	    (long long)st.st_size, r, errno);
	close(fd);
	free(buf);
	if (r != st.st_size)
		return -1;

	if (write_str("/sys/fs/selinux/enforce", "0"))
		out("PROBE_SELINUX permissive=fail errno=%d\n", errno);
	if (probe_read_file("/sys/fs/selinux/enforce", b, sizeof(b)) > 0)
		out("PROBE_SELINUX enforce=%s", b);
	if (probe_read_file("/sys/fs/selinux/policyvers", b, sizeof(b)) > 0)
		out("PROBE_SELINUX policyvers=%s", b);

	errno = 0;
	r = setxattr("/sys/devices/virtual", "security.selinux", ctx,
		     strlen(ctx), 0);
	out("PROBE_SELINUX setxattr_devices_virtual rc=%d errno=%d\n", r,
	    errno);
	errno = 0;
	r = setxattr("/sys/class/wakeup", "security.selinux", ctx,
		     strlen(ctx), 0);
	out("PROBE_SELINUX setxattr_class_wakeup rc=%d errno=%d\n", r, errno);
	n = getxattr("/sys/devices/virtual", "security.selinux", b,
		     sizeof(b) - 1);
	if (n > 0) {
		b[n] = 0;
		out("PROBE_SELINUX getxattr_devices_virtual=%s\n", b);
	} else {
		out("PROBE_SELINUX getxattr_devices_virtual=fail errno=%d\n",
		    errno);
	}
	return 0;
}

static int probe_memcg(void)
{
	struct probe_ws base;
	int i, viol = 0, ret_err = 0, killed = 0, rc;

	probe_set_nofile(32768);
	if (probe_mount_cgroup())
		return 1;
	probe_ws_count(&base);
	out("PROBE_MEMCG base total=%d ep=%d other=%d truncated=%d\n",
	    base.total, base.ep, base.other, base.truncated);
	for (i = 0; i < 20; i++) {
		int v = 0;

		rc = probe_memcg_one(i, &base, &v, &ret_err, &killed);
		if (rc == -1)
			out("PROBE_MEMCG trial=%d harness_error\n", i);
		if (v)
			viol++;
	}
	probe_memcg_pressure();
	out("PROBE_MEMCG summary trials=20 violations=%d ret_err=%d"
	    " child_killed=%d base_total=%d base_ep=%d\n",
	    viol, ret_err, killed, base.total, base.ep);
	return viol ? 1 : 0;
}

static int probe_memcg_selinux(void)
{
	if (probe_selinux_load())
		out("PROBE_MEMCG_SELINUX selinux_setup=fail continuing\n");
	return probe_memcg();
}

static int probe_memcg_selinux_oom(void)
{
	probe_oom_protect = 0;
	if (probe_selinux_load())
		out("PROBE_MEMCG_SELINUX_OOM selinux_setup=fail continuing\n");
	return probe_memcg();
}

/* Control: the same SELinux-labelled sysfs tree, no memory cgroup limit. */
static int probe_selinux_mod(void)
{
	struct probe_ws base, ws;
	int i, bad = 0, err = 0;

	if (probe_selinux_load())
		return 1;
	probe_ws_count(&base);
	out("PROBE_SELINUX_MOD base total=%d ep=%d other=%d\n", base.total,
	    base.ep, base.other);
	for (i = 0; i < 20; i++) {
		int epfd, pfd[2], r;
		struct epoll_event ev;
		unsigned int fi;

		if (probe_epoll_setup(&epfd, pfd, 0)) {
			out("PROBE_SELINUX_MOD trial=%d setup_fail errno=%d\n", i,
			    errno);
			bad++;
			continue;
		}
		memset(&ev, 0, sizeof(ev));
		ev.events = EPOLLIN | EPOLLWAKEUP;
		ev.data.u64 = 0x1234;
		errno = 0;
		r = epoll_ctl(epfd, EPOLL_CTL_MOD, pfd[0], &ev);
		probe_ws_count(&ws);
		fi = probe_fdinfo_events(epfd);
		if (r == 0 && ws.ep == base.ep + 1 &&
		    ws.other == base.other + 1) {
			out("PROBE_SELINUX_MOD trial=%d ret=0 ep=%d other=%d"
			    " fdinfo_events=%x sources=ok\n", i, ws.ep,
			    ws.other, fi);
		} else {
			bad++;
			err++;
			out("PROBE_SELINUX_MOD trial=%d ret=%d errno=%d ep=%d"
			    " other=%d fdinfo_events=%x sources=bad\n", i, r,
			    errno, ws.ep, ws.other, fi);
		}
		close(pfd[0]);
		close(pfd[1]);
		close(epfd);
	}
	out("PROBE_SELINUX_MOD summary trials=20 bad=%d errors=%d\n", bad,
	    err);
	return bad ? 1 : 0;
}

/* ------------------------------------------------------------------ */
/* pending-fatal-signal probe                                          */
/*
 * Stop the child at epoll_ctl(2) syscall entry with ptrace, queue SIGKILL,
 * and let the syscall body run.  SIGKILL stays pending until the syscall
 * returns to user mode, so this is the one way to run the allocation path
 * with fatal_signal_pending(current) set without fault injection.  The
 * parent keeps the epoll fd open, so the wakeup sources survive the child.
 */
static int probe_sigkill_once(int trial, const struct probe_ws *base,
			      int *viol, int *unconfirmed)
{
	int epfd, pfd[2], status;
	pid_t pid;
	struct user_regs_struct regs;
	struct probe_ws ws;
	unsigned int ev;
	int entry = 0, got_ret = 0, stop_kind = 0;
	long ret = -999;

	*viol = 0;
	if (probe_epoll_setup(&epfd, pfd, 0))
		return -1;
	pid = fork();
	if (pid == 0) {
		struct epoll_event ev2;

		if (ptrace(PTRACE_TRACEME, 0, 0, 0))
			_exit(2);
		raise(SIGSTOP);
		memset(&ev2, 0, sizeof(ev2));
		ev2.events = EPOLLIN | EPOLLWAKEUP;
		ev2.data.u64 = 0x1234;
		errno = 0;
		epoll_ctl(epfd, EPOLL_CTL_MOD, pfd[0], &ev2);
		_exit(0);
	}
	if (pid < 0) {
		close(epfd);
		return -1;
	}
	if (waitpid(pid, &status, 0) < 0 || !WIFSTOPPED(status)) {
		out("PROBE_SIGKILL trial=%d tracee_stop=fail status=%x\n",
		    trial, status);
		goto out;
	}
	ptrace(PTRACE_SETOPTIONS, pid, 0,
	       (void *)(long)PTRACE_O_TRACESYSGOOD);
	for (;;) {
		if (ptrace(PTRACE_SYSCALL, pid, 0, 0) < 0)
			break;
		if (waitpid(pid, &status, 0) < 0)
			break;
		if (WIFEXITED(status) || WIFSIGNALED(status)) {
			stop_kind = 1;
			break;
		}
		if (!WIFSTOPPED(status))
			break;
		if (WSTOPSIG(status) == (SIGTRAP | 0x80)) {
			if (ptrace(PTRACE_GETREGS, pid, 0, &regs) < 0)
				break;
			if (!entry) {
				if ((long)regs.orig_rax == (long)SYS_epoll_ctl) {
					entry = 1;
					kill(pid, SIGKILL);
				}
			} else {
				ret = (long)regs.rax;
				got_ret = 1;
				stop_kind = 2;
				ptrace(PTRACE_CONT, pid, 0, 0);
				break;
			}
		} else if (entry && WSTOPSIG(status) == SIGKILL) {
			if (ptrace(PTRACE_GETREGS, pid, 0, &regs) == 0) {
				ret = (long)regs.rax;
				got_ret = 1;
			}
			stop_kind = 3;
			ptrace(PTRACE_CONT, pid, 0, 0);
			break;
		}
	}
	kill(pid, SIGKILL);
	waitpid(pid, &status, 0);

	ev = probe_fdinfo_events(epfd);
	probe_ws_count(&ws);
	if (got_ret && ret == 0 && ws.ep == base->ep + 1 &&
	    ws.other == base->other + 1) {
		out("PROBE_SIGKILL trial=%d entry=1 ret=0 ep=%d other=%d"
		    " fdinfo_events=%x sources=ok stop=%d\n",
		    trial, ws.ep, ws.other, ev, stop_kind);
	} else if (got_ret && ret == 0) {
		*viol = 1;
		out("PROBE_SIGKILL trial=%d VERDICT=VIOL entry=1 ret=0 ep=%d"
		    " other=%d fdinfo_events=%x stop=%d\n",
		    trial, ws.ep, ws.other, ev, stop_kind);
	} else if (entry && (ev & EPOLLWAKEUP) &&
		   !(ws.ep == base->ep + 1 && ws.other == base->other + 1)) {
		*unconfirmed = 1;
		out("PROBE_SIGKILL trial=%d UNCONFIRMED entry=1 got_ret=%d"
		    " ret=%ld ep=%d other=%d fdinfo_events=%x stop=%d\n",
		    trial, got_ret, ret, ws.ep, ws.other, ev, stop_kind);
	} else {
		out("PROBE_SIGKILL trial=%d entry=%d got_ret=%d ret=%ld ep=%d"
		    " other=%d fdinfo_events=%x stop=%d\n",
		    trial, entry, got_ret, ret, ws.ep, ws.other, ev,
		    stop_kind);
	}
out:
	close(epfd);
	return 0;
}

static int probe_sigkill(void)
{
	struct probe_ws base;
	int i, viol = 0, unconfirmed = 0;

	for (i = 0; i < 10; i++) {
		probe_ws_count(&base);
		if (base.total < 0) {
			out("PROBE_SIGKILL wakeup_sources=unreadable\n");
			return 1;
		}
		if (!i)
			out("PROBE_SIGKILL base total=%d ep=%d other=%d\n",
			    base.total, base.ep, base.other);
		probe_sigkill_once(i, &base, &viol, &unconfirmed);
	}
	out("PROBE_SIGKILL summary trials=10 violations=%d unconfirmed=%d\n",
	    viol, unconfirmed);
	return viol ? 1 : 0;
}

/* ------------------------------------------------------------------ */
/* capability probe                                                    */
static int probe_cap(void)
{
	int epfd, pfd[2], epfd2, pfd2[2], res[2], status;
	pid_t pid;
	struct probe_msg m;
	struct probe_ws base, ws;
	unsigned int ev;
	int n;

	probe_ws_count(&base);
	out("PROBE_CAP base total=%d ep=%d other=%d\n", base.total, base.ep,
	    base.other);

	/* Root control: the same MOD on an equivalent epoll. */
	if (probe_epoll_setup(&epfd2, pfd2, 0)) {
		out("PROBE_CAP root_setup=fail errno=%d\n", errno);
		return 1;
	}
	{
		struct epoll_event ev2;

		memset(&ev2, 0, sizeof(ev2));
		ev2.events = EPOLLIN | EPOLLWAKEUP;
		ev2.data.u64 = 0x1234;
		errno = 0;
		n = epoll_ctl(epfd2, EPOLL_CTL_MOD, pfd2[0], &ev2);
		ev = probe_fdinfo_events(epfd2);
		probe_ws_count(&ws);
		out("PROBE_CAP root ret=%d errno=%d fdinfo_events=%x ep=%d"
		    " other=%d\n", n, errno, ev, ws.ep, ws.other);
	}
	close(epfd2);
	close(pfd2[0]);
	close(pfd2[1]);

	if (probe_epoll_setup(&epfd, pfd, 0)) {
		out("PROBE_CAP child_setup=fail errno=%d\n", errno);
		return 1;
	}
	if (pipe(res)) {
		close(epfd);
		return 1;
	}
	pid = fork();
	if (pid == 0) {
		struct epoll_event ev2;

		close(res[0]);
		memset(&m, 0, sizeof(m));
		if (setuid(65534)) {
			m.stage = -1;
			m.err = errno;
			write(res[1], &m, sizeof(m));
			_exit(3);
		}
		memset(&ev2, 0, sizeof(ev2));
		ev2.events = EPOLLIN | EPOLLWAKEUP;
		ev2.data.u64 = 0x1234;
		errno = 0;
		m.stage = 2;
		m.fd = epfd;
		m.events = ev2.events;
		m.ret = epoll_ctl(epfd, EPOLL_CTL_MOD, pfd[0], &ev2);
		m.err = errno;
		write(res[1], &m, sizeof(m));
		_exit(0);
	}
	close(res[1]);
	if (pid < 0) {
		close(epfd);
		close(res[0]);
		return 1;
	}
	if (probe_read_msg_timeout(res[0], &m, 5000) == 0 && m.stage == 2) {
		ev = probe_fdinfo_events(epfd);
		probe_ws_count(&ws);
		out("PROBE_CAP unprivileged ret=%ld errno=%d fdinfo_events=%x"
		    " ep=%d other=%d\n", m.ret, m.err, ev, ws.ep, ws.other);
	} else {
		out("PROBE_CAP unprivileged setuid=fail stage=%d errno=%d\n",
		    m.stage, m.err);
	}
	waitpid(pid, &status, 0);
	close(res[0]);
	close(epfd);
	close(pfd[0]);
	close(pfd[1]);
	return 0;
}

/* ------------------------------------------------------------------ */
/* sysfs/device-registration probe                                     */
static int probe_sysfs(void)
{
	enum { N = 3000 };
	int (*pfds)[2] = calloc(N, 2 * sizeof(int));
	int *efds = calloc(N, sizeof(int));
	struct probe_ws base, ws;
	unsigned long t0;
	int i, created = 0, failed = 0, r;
	int epfd, pfd[2];

	probe_set_nofile(65536);
	if (!pfds || !efds) {
		out("PROBE_SYSFS alloc=fail\n");
		return 1;
	}
	probe_ws_count(&base);
	out("PROBE_SYSFS base total=%d ep=%d other=%d n=%d\n",
	    base.total, base.ep, base.other, N);
	t0 = now_ms();
	for (i = 0; i < N; i++) {
		if (probe_epoll_setup(&epfd, pfd, 1)) {
			int e = errno;

			failed++;
			if (failed <= 3)
				out("PROBE_SYSFS add_fail i=%d errno=%d\n", i, e);
			continue;
		}
		efds[created] = epfd;
		pfds[created][0] = pfd[0];
		pfds[created][1] = pfd[1];
		created++;
	}
	probe_ws_count(&ws);
	out("PROBE_SYSFS created=%d failed=%d elapsed_ms=%lu"
	    " ep=%d other=%d total=%d truncated=%d\n",
	    created, failed, now_ms() - t0, ws.ep, ws.other, ws.total,
	    ws.truncated);
	for (i = 0; i < created; i++) {
		close(pfds[i][0]);
		close(pfds[i][1]);
		close(efds[i]);
	}
	probe_ws_count(&ws);
	out("PROBE_SYSFS after_close ep=%d other=%d total=%d\n", ws.ep,
	    ws.other, ws.total);

	/* A read-only sysfs mount and a tmpfs mounted over the class
	 * directory must not affect the kernel's internal kernfs tree. */
	r = mount(NULL, "/sys", NULL, MS_REMOUNT | MS_RDONLY, NULL);
	out("PROBE_SYSFS remount_sys_ro rc=%d errno=%d\n", r, errno);
	if (probe_epoll_setup(&epfd, pfd, 1)) {
		out("PROBE_SYSFS ro_add=fail errno=%d\n", errno);
	} else {
		probe_ws_count(&ws);
		out("PROBE_SYSFS ro_add=ok ep=%d other=%d\n", ws.ep, ws.other);
		close(epfd);
		close(pfd[0]);
		close(pfd[1]);
	}
	if (r == 0)
		mount(NULL, "/sys", NULL, MS_REMOUNT, NULL);

	r = rmdir("/sys/class/wakeup/wakeup0");
	out("PROBE_SYSFS rmdir_wakeup0 rc=%d errno=%d\n", r, errno);

	mkdir_if_needed("/sys/class/wakeup", 0755);
	r = mount("tmpfs", "/sys/class/wakeup", "tmpfs", 0, "size=1m");
	out("PROBE_SYSFS mount_tmpfs_over_class rc=%d errno=%d\n", r, errno);
	if (r == 0) {
		int fd = open("/sys/class/wakeup/wakeup0",
			      O_CREAT | O_WRONLY, 0644);

		out("PROBE_SYSFS create_wakeup0 fd=%d errno=%d\n", fd, errno);
		if (fd >= 0)
			close(fd);
		if (probe_epoll_setup(&epfd, pfd, 1)) {
			out("PROBE_SYSFS shadow_add=fail errno=%d\n", errno);
		} else {
			probe_ws_count(&ws);
			out("PROBE_SYSFS shadow_add=ok ep=%d other=%d\n",
			    ws.ep, ws.other);
			close(epfd);
			close(pfd[0]);
			close(pfd[1]);
		}
		umount2("/sys/class/wakeup", MNT_DETACH);
	}
	free(pfds);
	free(efds);
	return 0;
}

static int run_probe(const char *name)
{
	if (!strcmp(name, "probe_memcg"))
		return probe_memcg();
	if (!strcmp(name, "probe_memcg_selinux"))
		return probe_memcg_selinux();
	if (!strcmp(name, "probe_memcg_selinux_oom"))
		return probe_memcg_selinux_oom();
	if (!strcmp(name, "probe_selinux_mod"))
		return probe_selinux_mod();
	if (!strcmp(name, "probe_selinux"))
		return probe_selinux_load() ? 1 : 0;
	if (!strcmp(name, "probe_sigkill"))
		return probe_sigkill();
	if (!strcmp(name, "probe_cap"))
		return probe_cap();
	if (!strcmp(name, "probe_sysfs"))
		return probe_sysfs();
	out("PROBE_ERROR unknown=%s\n", name);
	return 2;
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
	if (!strncmp(selected, "probe_", 6)) {
		int pr = run_probe(selected);

		out("PROBE_DONE probe=%s rc=%d\n", selected, pr);
		out("HARNESS_DONE\n");
		sync();
		reboot(RB_POWER_OFF);
		for (;;)
			pause();
	}
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
