CC = clang
CFLAGS = -g -O0 -Wall -Wextra
TARGET = stress_test_phase1c

all: $(TARGET)

$(TARGET): main.c
	$(CC) $(CFLAGS) -o $@ $<

clean:
	rm -rf $(TARGET) $(TARGET).dSYM

.PHONY: all clean
