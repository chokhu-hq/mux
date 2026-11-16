cask "mux" do
  version :latest
  sha256 :no_check

  url "https://github.com/chokhu-hq/mux/releases/download/experimental/mux-macos-universal.zip"
  name "Mux"
  desc "Native harness for the Codex CLI, Claude Code and OpenCode"
  homepage "https://github.com/chokhu-hq/mux"

  auto_updates true
  depends_on macos: ">= :big_sur"

  app "Mux.app"

  # the app isn't notarised yet, so gatekeeper would call it damaged
  postflight do
    system_command "/usr/bin/xattr", args: ["-dr", "com.apple.quarantine", "#{appdir}/Mux.app"]
  end

  zap trash: "~/Library/Application Support/mux"
end
