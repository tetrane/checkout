checkout
========

Tool to recursively update submodules in no-brain mode.

# Getting the binary

Each release produces new binaries:

- [Linux (`glibc`, dynamic binary)](https://github.com/tetrane/checkout/releases/latest/download/checkout)
- [Linux (`musl`, static binary)](https://github.com/tetrane/checkout/releases/latest/download/checkout-static)
- [Windows](https://github.com/tetrane/checkout/releases/latest/download/checkout.exe)

# Adding to CI

## Linux

```yml
- apt update && apt install -y --no-install-recommends curl 
- |
         curl --location --output /usr/local/bin/checkout \
              "https://github.com/tetrane/checkout/releases/latest/download/checkout-static"
- chmod +x /usr/local/bin/checkout
- checkout
```

## Windows

```yml
- wget -OutFile checkout.exe
             "https://github.com/tetrane/checkout/releases/latest/download/checkout.exe"
- checkout.exe
```

# Building from source

```
cargo build --release
```

will generate the `target/release/checkout` binary.

# Usage

```
checkout --help
```
