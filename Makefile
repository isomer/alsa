CFLAGS=-Wall -Wextra -Wmissing-prototypes -Wstrict-prototypes -Og -mtune=native

LDLIBS=-lasound -lm

all: alsa alsa2
