# Homebrew formula for Kite.
#
# A formula, not a cask: these are command-line binaries. It downloads the
# release archive for the running platform, checks it against the checksum the
# release published, and installs two files. Nothing is compiled and nothing is
# run from the archive.
#
# The checksums below are placeholders until a release fills them in.
# `packaging/render.sh <version>` writes the real ones from that release's own
# `SHA256SUMS`, which is the only place they should ever come from.
class Kite < Formula
  desc "Small, explicit programming language for application software, with WebAssembly as its primary target"
  homepage "https://github.com/channyeintun/kite-lang"
  version "0.0.0"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/channyeintun/kite-lang/releases/download/v#{version}/kite-v#{version}-aarch64-apple-darwin.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000000"
    end
    on_intel do
      url "https://github.com/channyeintun/kite-lang/releases/download/v#{version}/kite-v#{version}-x86_64-apple-darwin.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000000"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/channyeintun/kite-lang/releases/download/v#{version}/kite-v#{version}-aarch64-unknown-linux-musl.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000000"
    end
    on_intel do
      url "https://github.com/channyeintun/kite-lang/releases/download/v#{version}/kite-v#{version}-x86_64-unknown-linux-musl.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000000"
    end
  end

  def install
    bin.install "kitec"
    bin.install "kite-lsp"
    doc.install "README.md", "SPECIFICATION.md"
    prefix.install "LICENSE"
  end

  # A test that compiles and runs a program, rather than one that prints a
  # version string. A compiler that starts and cannot compile has passed the
  # wrong test.
  test do
    (testpath/"hello.kite").write <<~KITE
      fn main() {
          io.print("hello from brew")
      }
    KITE
    assert_equal "hello from brew\n", shell_output("#{bin}/kitec run hello.kite")
  end
end
