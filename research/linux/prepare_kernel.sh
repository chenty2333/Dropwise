#!/bin/bash
# Build the fault-injection kernel used by this prototype.
#
# The Linux source is kept outside the Dropwise repository.  This script adds a
# detached worktree at a release tag from the caller's main kernel repository;
# it does not change any existing worktree, branch or commit.
#
# Override with: MAIN_REPO=... TAG=... WORK=... ./prepare_kernel.sh
set -euo pipefail

MAIN_REPO=${MAIN_REPO:-$HOME/Desktop/linux/linux}
TAG=${TAG:-v6.19}
WORK=${WORK:-$HOME/dropwise-linux-work}
SRC=$WORK/linux
BUILD=$WORK/build

mkdir -p "$WORK"
if [ ! -e "$SRC/.git" ]; then
	git -C "$MAIN_REPO" worktree add --detach "$SRC" "$TAG"
else
	echo "using existing source worktree: $SRC" >&2
fi

mkdir -p "$BUILD"
cd "$SRC"
make O="$BUILD" defconfig
./scripts/config --file "$BUILD/.config" \
	-e FAULT_INJECTION -e FAILSLAB -e FAIL_PAGE_ALLOC \
	-e FAULT_INJECTION_DEBUG_FS -e FAIL_FUTEX \
	-e FUNCTION_ERROR_INJECTION -e FAIL_FUNCTION -e FAIL_SKB_REALLOC \
	-e TLS -e OVERLAY_FS -e CRYPTO -e CRYPTO_AES -e CRYPTO_GCM \
	-e DEBUG_FS -e DEBUG_KERNEL \
	-e 9P_FS -e NET_9P -e NET_9P_VIRTIO -e NET_9P_FD \
	-e VIRTIO -e VIRTIO_PCI -e VIRTIO_MMIO -e PCI \
	-e TMPFS -e SHMEM -e EPOLL -e UNIX -e PROC_FS -e SYSFS \
	-e DEVTMPFS -e DEVTMPFS_MOUNT \
	-e BINFMT_ELF -e SERIAL_8250 -e SERIAL_8250_CONSOLE -e TTY \
	-e PRINTK -e BLK_DEV_INITRD -e RD_GZIP -e FUTEX \
	-e MEMFD_CREATE -e SIGNALFD -e TIMERFD -e EVENTFD \
	-e POSIX_MQUEUE -e POSIX_MQUEUE_SYSCTL -e INET -e IPV6 \
	-d MODULES -d DEBUG_INFO_BTF -d DEBUG_INFO_DWARF5 \
	-d DEBUG_INFO_DWARF4 -e DEBUG_INFO_NONE -d WERROR \
	-d RANDOMIZE_BASE -d KEXEC -d KEXEC_FILE \
	--set-str SYSTEM_TRUSTED_KEYS "" --set-str SYSTEM_REVOCATION_KEYS ""
make O="$BUILD" olddefconfig
make O="$BUILD" -j"$(nproc)" bzImage

echo
echo "kernel: $BUILD/arch/x86/boot/bzImage"
grep -E '^CONFIG_(FAULT_INJECTION|FAILSLAB|FAIL_PAGE_ALLOC|FAULT_INJECTION_DEBUG_FS|FAIL_FUTEX|DEBUG_FS|TMPFS|EPOLL|UNIX|FILE_LOCKING)=' "$BUILD/.config" || true
