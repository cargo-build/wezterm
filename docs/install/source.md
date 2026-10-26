## Installing from source

If your system isn't covered by the pre-built packages then you can build it
for yourself.  WezTerm should run on any modern unix as well as Windows 10 and
macOS.

* Install `rustup` to get the `rust` compiler installed on your system.
  [Install rustup](https://rust-lang.org/tools/install/).
* Rust version 1.71 or later is required
* Build in release mode: `cargo build --release`
* Run it via either `cargo run --release --bin wezterm` or `target/release/wezterm`

You will need a collection of support libraries; the [`get-deps`](https://github.com/wezterm/wezterm/blob/main/get-deps) script will
attempt to install them for you.  If it doesn't know about your system,
[please contribute instructions!](https://github.com/wezterm/wezterm/blob/main/CONTRIBUTING.md).

If you don't plan to submit a pull request to the wezterm repo, you can
download a smaller source tarball using these steps:

```console
$ curl https://sh.rustup.rs -sSf | sh -s
$ curl -LO {{ src_stable }}
$ tar -xzf {{ src_stable_asset }}
$ cd {{ src_stable_dir }}
$ ./get-deps
$ cargo build --release
$ cargo run --release --bin wezterm -- start
```

Alternatively, use the full git repo:

```console
$ curl https://sh.rustup.rs -sSf | sh -s
$ git clone --depth=1 --branch=main --recursive https://github.com/wezterm/wezterm.git
$ cd wezterm
$ git submodule update --init --recursive
$ ./get-deps
$ cargo build --release
$ cargo run --release --bin wezterm -- start
```

**If you get an error about zlib then you most likely didn't initialize the submodules;
take a closer look at the instructions!**

### Optional Linux PTY io_uring support

The default build uses the existing native PTY I/O. On Linux, `io-uring` selects
completion-based reads and writes for local PTYs while retaining the existing
terminal parser, renderer and blocking reader/writer interfaces:

```console
$ cargo build --release -p wezterm-gui --features io-uring
$ cargo build --release -p wezterm-mux-server --features io-uring
$ cargo build --release -p wezterm --features io-uring
```

The feature adds to the GUI's existing Wayland and font defaults; disabling
default features is unnecessary. It must be enabled in the process that owns
the PTY, including a separate mux server when one is used. SSH, serial ports and
remote mux transports retain their existing implementations.

This option requires Linux 5.6 or newer with io_uring permitted by the host's
security policy. Initialization failures are reported without falling back to
native PTY I/O. On other operating systems, the feature retains native PTY I/O
and does not build the Linux dependency. The adapter uses independent rings for
input and output and retains the current per-pane threads; no performance gain
is implied.

### Building without Wayland support on Unix systems

By default, support for both X11 and Wayland is included on Unix systems.
If your distribution has X11 but not Wayland, then you can build WezTerm without
Wayland support by changing the `build` invocation:

```console
$ cargo build --release --no-default-features --features vendored-fonts
```

Building without X11 is not supported.

### Building on Windows

When installing Rust, you must use select the MSVC version of Rust. It is the
only supported way to build wezterm.

On Windows, instead of using `get-deps`, the only other dependency that you need is
[Strawberry Perl](https://strawberryperl.com). You must ensure that you have
your `PATH` environment set up to find that particular `perl.exe` ahead of any
other perl that you may have installed on your system. This particular version
of perl is required to build openssl on Windows.

```console
$ set PATH=c:\Strawberry\perl\bin;%PATH%
```

