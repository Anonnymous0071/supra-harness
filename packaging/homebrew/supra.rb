class Supra < Formula
  desc "Peer-validated coding-agent harness CLI"
  homepage "https://github.com/Anonnymous0071/supra-harness"
  version "0.2.0"
  license "MIT"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/Anonnymous0071/supra-harness/releases/download/v0.2.0/supra-aarch64-apple-darwin.tar.gz"
      sha256 "REPLACE_WITH_AARCH64_APPLE_SHA256"
    else
      url "https://github.com/Anonnymous0071/supra-harness/releases/download/v0.2.0/supra-x86_64-apple-darwin.tar.gz"
      sha256 "REPLACE_WITH_X86_64_APPLE_SHA256"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/Anonnymous0071/supra-harness/releases/download/v0.2.0/supra-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "REPLACE_WITH_AARCH64_LINUX_SHA256"
    else
      url "https://github.com/Anonnymous0071/supra-harness/releases/download/v0.2.0/supra-x86_64-unknown-linux-musl.tar.gz"
      sha256 "REPLACE_WITH_X86_64_MUSL_SHA256"
    end
  end

  def install
    bin.install "supra"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/supra --version")
  end
end
