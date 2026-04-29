# Homebrew formula skeleton for crabcc.
#
# Drop-in for a tap repo at peterlodri-sec/homebrew-tap. The release.yml
# workflow updates the `version`, `url`, and `sha256` fields on each tag push;
# this file is the source of truth before that automation lands.
#
# Install:
#   brew tap peterlodri-sec/tap
#   brew install crabcc
#
# Or, ad-hoc from this checkout:
#   brew install --build-from-source ./contrib/homebrew/crabcc.rb

class Crabcc < Formula
  desc     "Symbol index for AI coding agents — 47-4400× faster than grep -rn"
  homepage "https://github.com/peterlodri-sec/crabcc"
  version  "0.1.0"   # bumped by release.yml on each tag

  if OS.mac?
    if Hardware::CPU.arm?
      url    "https://github.com/peterlodri-sec/crabcc/releases/download/v#{version}/crabcc-v#{version}-aarch64-apple-darwin.tar.gz"
      sha256 "REPLACE_ME_AARCH64_DARWIN"
    else
      url    "https://github.com/peterlodri-sec/crabcc/releases/download/v#{version}/crabcc-v#{version}-x86_64-apple-darwin.tar.gz"
      sha256 "REPLACE_ME_X86_64_DARWIN"
    end
  elsif OS.linux?
    if Hardware::CPU.arm?
      url    "https://github.com/peterlodri-sec/crabcc/releases/download/v#{version}/crabcc-v#{version}-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "REPLACE_ME_AARCH64_LINUX"
    else
      url    "https://github.com/peterlodri-sec/crabcc/releases/download/v#{version}/crabcc-v#{version}-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "REPLACE_ME_X86_64_LINUX"
    end
  end

  license "MIT"

  def install
    # The release tarball contains a versioned subdir whose top-level holds the
    # binary, README, LICENSE, CHANGELOG. Walk into it before installing.
    Dir.glob("**/crabcc").each { |b| bin.install b; break }
    Dir.glob("**/crabcc.1").each { |m| man1.install m; break }
    Dir.glob("**/README.md").each { |r| doc.install r; break }
    Dir.glob("**/CHANGELOG.md").each { |c| doc.install c; break }
  end

  test do
    # Smoke test: index a tiny TS file, verify sym lookup returns the symbol.
    (testpath/"a.ts").write <<~TS
      export function hello(name: string) { return name; }
    TS
    system "#{bin}/crabcc", "index", "--root", testpath.to_s
    output = shell_output("#{bin}/crabcc --root #{testpath} sym hello")
    assert_match(/"name":"hello"/, output)
  end
end
