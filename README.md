# MYORE CLI

A Modification ORE CLI From Official [ore-cli](https://github.com/regolith-labs/ore-cli).

## 🚀 Install

To install the CLI, use [cargo](https://doc.rust-lang.org/cargo/getting-started/installation.html):

```sh
cargo install myore-cli
```

### 🛠️ Dependencies

If you run into issues during installation, please install the following dependencies for your operating system and try again:

#### Linux
```
sudo apt-get install openssl pkg-config libssl-dev
```

#### MacOS (using [Homebrew](https://brew.sh/))
```
brew install openssl pkg-config

# If you encounter issues with OpenSSL, you might need to set the following environment variables:
export PATH="/usr/local/opt/openssl/bin:$PATH"
export LDFLAGS="-L/usr/local/opt/openssl/lib"
export CPPFLAGS="-I/usr/local/opt/openssl/include"
```

#### Windows (using [Chocolatey](https://chocolatey.org/))
```
choco install openssl pkgconfiglite
```

## 🔨 Build

To build the codebase from scratch, checkout the repo and use cargo to build:

```sh
cargo build --release
```

## 📖 Help

You can use the `-h` flag on any command to pull up a help menu with documentation:

```sh
myore -h
```

## ☕ Support

If you find this project useful and would like to support its development, consider buying me a coffee:

```
Solana Address: 27Tw4MutGD9M2u1biQw4B6J3b6mfB98yCbpZuu8PzoTG
```

Thank you for your support!