# mpvpaper-rs Makefile
#
# `cargo build` must run as your normal user (rustup's default toolchain isn't
# configured for root), whereas writing to /usr/local/bin needs root. Workflow:
#     make              # as your user
#     sudo make install # as root, copies the already-built binary

PREFIX ?= /usr/local
BINDIR ?= $(PREFIX)/bin
BIN    := mpvpaper-rs
TARGET := target/release/$(BIN)

.PHONY: all build install uninstall clean

all: build

build:
	cargo build --release

install:
	@if [ ! -f "$(TARGET)" ]; then \
		echo "$(TARGET) not found. Build it first as your normal user:"; \
		echo "    make"; \
		echo "then re-run: sudo make install"; \
		exit 1; \
	fi
	install -Dm755 $(TARGET) $(DESTDIR)$(BINDIR)/$(BIN)
	@echo "Installed $(BIN) to $(DESTDIR)$(BINDIR)/$(BIN)"

uninstall:
	rm -f $(DESTDIR)$(BINDIR)/$(BIN)
	@echo "Removed $(DESTDIR)$(BINDIR)/$(BIN)"

clean:
	cargo clean
