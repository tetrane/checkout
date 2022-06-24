checkout
========

Tool to recursively update submodules in no-brain mode.

# Getting the binary

Each commit to the master branch produces new binaries:

- [Linux](../-/jobs/artifacts/master/file/target/release/checkout?job=build)
- [Windows](../-/jobs/artifacts/master/file/target/x86_64-pc-windows-gnu/release/checkout.exe?job=build)

# Adding to CI

## Linux

```yml
- apt update && apt install -y --no-install-recommends curl 
- export CURL_CA_BUNDLE="$CI_SERVER_TLS_CA_FILE"
- |
         curl --location --output /usr/local/bin/checkout \
              "${CI_API_V4_URL}/projects/tetrane-public%2Ftools%2Fcheckout/jobs/artifacts/master/raw/target/release/checkout?job=build"
- chmod +x /usr/local/bin/checkout
- checkout
```

## Windows

```yml
- wget -OutFile checkout.exe
             "${CI_API_V4_URL}/projects/tetrane-public%2Ftools%2Fcheckout/jobs/artifacts/master/raw/target/x86_64-pc-windows-gnu/release/checkout.exe?job=build"
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
