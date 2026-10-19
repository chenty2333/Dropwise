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

struct outcome {
	int mode;		/* 'P' pre, 'O' post, 'F' fault */
	long ret;
	int err;
	int injected;
	unsigned long readback;
	int hang;
	int got_kid;
	struct kid_report kid;
	long long aux[8];
	char obs[MAX_OBS];
};

struct target {
	const char *name;
	long full;		/* expected return value of a full success */
	int (*setup)(void *ctx);
	long (*p)(void *ctx, int *err);
	void (*before_survivor)(void *ctx);
	void (*survivor)(void *ctx, int wfd);
	void (*observer)(void *ctx, const struct kid_report *kid,
			 struct outcome *o, char *obs, size_t cap);
	int (*partial_ok)(void *ctx, struct outcome *o);
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

static int scm_setup(void *p)
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

static int pipe_setup(void *p)
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

static int tmpfs_setup(void *p)
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

static int epoll_setup(void *p)
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

static int lock_setup(void *p)
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

static int accept_setup(void *p)
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
/* targets table                                                       */

static const struct target t_scm = {
	.name = "scm_rights",
	.full = 1,
	.setup = scm_setup,
	.p = scm_p,
	.survivor = scm_survivor,
	.observer = scm_observer,
	.teardown = scm_teardown,
};

static const struct target t_pipe = {
	.name = "pipe_write",
	.full = PIPE_FULL,
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
	.setup = lock_setup,
	.p = lock_p,
	.survivor = lock_survivor,
	.observer = lock_observer,
	.teardown = lock_teardown,
};

static const struct target t_accept = {
	.name = "accept4_unix",
	.full = -2,	/* success is any non-negative fd */
	.setup = accept_setup,
	.p = accept_p,
	.survivor = accept_survivor,
	.observer = accept_observer,
	.teardown = accept_teardown,
};

static const struct target *all_targets[] = {
	&t_scm, &t_pipe, &t_tmpfs, &t_epoll, &t_lock, &t_accept, NULL
};

/* ------------------------------------------------------------------ */
/* trial execution                                                     */

static void kmsg_mark(const char *tag, const char *name, unsigned int n)
{
	if (kmsg_fd < 0)
		return;
	dprintf(kmsg_fd, "%s target=%s n=%u\n", tag, name, n);
}

static int run_trial(const struct target *t, int mode, unsigned int n,
		     struct outcome *o)
{
	int rp[2] = { -1, -1 };
	pid_t pid;
	struct pollfd pfd;
	int pr, status, ret = -1;

	memset(o, 0, sizeof(*o));
	o->mode = mode;
	fn_set(0);
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
	if (t->setup(&C) < 0) {
		out("HARNESS_ERROR target=%s reason=setup errno=%d\n",
		    t->name, errno);
		goto out;
	}

	if (mode != 'P') {
		if (mode == 'F' || mode == 'R') {
			if (verbose)
				kmsg_mark("DWTRACE", t->name, n);
			if (fn_set(n) < 0) {
				out("HARNESS_ERROR target=%s reason=fn_set n=%u errno=%d\n",
				    t->name, n, errno);
				goto out;
			}
		}
		errno = 0;
		o->ret = t->p(&C, &o->err);
		if (o->ret < 0)
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
	} else {
		o->ret = 0;
		o->err = 0;
	}

	/* The survivor must never inherit an armed fail_nth. */
	fn_set(0);

	if (t->before_survivor)
		t->before_survivor(&C);

	pid = fork();
	if (pid < 0) {
		out("HARNESS_ERROR target=%s reason=fork errno=%d\n", t->name,
		    errno);
		goto out;
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
		ret = 1;
		goto out;
	}
	t->observer(&C, &o->kid, o, o->obs, sizeof(o->obs));
	ret = 1;
out:
	if (rp[0] >= 0)
		close(rp[0]);
	if (rp[1] >= 0)
		close(rp[1]);
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
			      struct outcome *o)
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
		{
			struct outcome dry;

			if (run_trial(t, 'D', 0, &dry) != 1)
				out("HARNESS_WARN target=%s dry_failed\n",
				    t->name);
		}
		child_rc = run_trial(t, mode, n, o);
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

static int run_target(const struct target *t)
{
	char pre[2][MAX_OBS], post[2][MAX_OBS];
	struct outcome o;
	int stable = 1;
	int i, n;
	int sites = 0, trials = 0, ok_pre = 0, ok_post = 0, ok_partial = 0;
	int violations = 0, unknown = 0;
	unsigned long started = now_ms();

	out("TARGET_BEGIN target=%s full=%ld\n", t->name, t->full);

	for (i = 0; i < 3; i++) {
		struct outcome po;
		long calls;

		if (run_trial_isolated(t, 'R', 1000, &po) != 1) {
			out("PROBE_ERROR target=%s sample=%d\n", t->name, i + 1);
			break;
		}
		calls = 1000 - (long)po.readback;
		out("PROBE target=%s sample=%d ret=%ld errno=%d calls=%ld readback=%lu\n",
		    t->name, i + 1, po.ret, po.err, calls, po.readback);
	}

	for (i = 0; i < 2; i++) {
		if (run_trial_isolated(t, 'P', 0, &o) != 1) {
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
		if (run_trial_isolated(t, 'O', 0, &o) != 1) {
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

	for (n = 1; n <= MAX_N; n++) {
		enum verdict v;

		{
			int rr = run_trial_isolated(t, 'F', (unsigned int)n, &o);

			if (rr < 0) {
				out("TRIAL_END target=%s n=%d reason=harness_error\n",
				    t->name, n);
				break;
			}
			if (rr == 0) {
				out("TRIAL_END target=%s n=%d reason=no_injection readback=%lu ret=%ld errno=%d\n",
				    t->name, n, o.readback, o.ret, o.err);
				break;
			}
		}
		sites++;
		trials++;
		v = judge(t, &o, pre[0], post[0], stable);
		out("TRIAL target=%s n=%d ret=%ld errno=%d injected=%d readback=%lu verdict=%s obs=%s\n",
		    t->name, n, o.ret, o.err, o.injected, o.readback,
		    vname(v), o.obs);
		switch (v) {
		case V_OK_PRE: ok_pre++; break;
		case V_OK_POST: ok_post++; break;
		case V_OK_PARTIAL: ok_partial++; break;
		case V_UNKNOWN: unknown++; break;
		default: violations++; break;
		}
		if (o.hang) {
			out("TRIAL_END target=%s n=%d reason=hang\n", t->name, n);
			break;
		}
	}
	out("SUMMARY target=%s sites=%d trials=%d ok_pre=%d ok_post=%d ok_partial=%d violations=%d unknown=%d stable=%d elapsed_ms=%lu\n",
	    t->name, sites, trials, ok_pre, ok_post, ok_partial, violations,
	    unknown, stable, now_ms() - started);
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

	{
		int fd = open("/dev/kmsg", O_WRONLY);

		if (fd >= 0)
			kmsg_fd = fd;
	}

	if (selftest() < 0) {
		out("HARNESS_DONE\n");
		sync();
		reboot(RB_POWER_OFF);
		for (;;)
			pause();
	}

	sel = selected;
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
