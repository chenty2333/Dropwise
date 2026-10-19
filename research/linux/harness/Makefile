CC ?= gcc
CFLAGS ?= -O2 -static -Wall -Wextra -Wno-unused-parameter
TARGET := ../build/harness

all: $(TARGET)

$(TARGET): dw_harness.c
	mkdir -p ../build
	$(CC) $(CFLAGS) -o $@ $<

clean:
	rm -f $(TARGET)

.PHONY: all clean
