date=2026-10-22T01:45:22.714899+09:00
cmd=/usr/local/bin/qemu-system-x86_64 -machine q35,accel=kvm -cpu host -smp 2 -m 1024 -kernel /home/ava/dropwise-linux-r4/bzImage-fixed -initrd /home/ava/Desktop/Dropwise/research/linux/mcast-delivery/build/initramfs.cpio.gz -append console=ttyS0 panic=-1 oops=panic quiet loglevel=4 mcast.label=v73-fixed -display none -serial stdio -monitor none -no-reboot
