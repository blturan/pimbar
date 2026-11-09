# Homebrew cask for pimbar. This repository doubles as its own tap:
#
#   brew tap blturan/pimbar https://github.com/blturan/pimbar
#   brew install --cask pimbar
#
# version and sha256 are kept current by the release workflow
# (.github/workflows/release.yml).
cask "pimbar" do
  version "0.6.0"
  sha256 "a63a88f74fa7fedd6a22cb85bcd675d33e1a58d4e3e451d7963a0388499a004d"

  url "https://github.com/blturan/pimbar/releases/download/v#{version}/pimbar_#{version}_aarch64.dmg"
  name "pimbar"
  desc "Menu bar app for Azure PIM role activation and approvals"
  homepage "https://github.com/blturan/pimbar"

  livecheck do
    url :url
    strategy :github_latest
  end

  depends_on arch: :arm64
  # Opening at login uses SMAppService, new in macOS 13.
  depends_on macos: :ventura
  # pimbar signs in to Azure with the Azure CLI's token.
  depends_on cask: "azure-cli"

  app "pimbar.app"

  uninstall quit: "dev.pimbar.desktop"

  zap trash: "~/Library/Application Support/dev.pimbar.desktop"
end
