// well known servers to start from, filled into the form so a path or token can be set before adding
pub(super) struct Preset {
    pub(super) name: &'static str,
    pub(super) label: &'static str,
    pub(super) detail: &'static str,
    pub(super) command: Option<&'static str>,
    pub(super) url: Option<&'static str>,
    pub(super) env: &'static [&'static str],
    pub(super) headers: &'static [(&'static str, &'static str)],
}

pub(super) const PRESETS: &[Preset] = &[
    Preset {
        name: "filesystem",
        label: "Filesystem",
        detail: "Read and write files in folders you pick",
        command: Some("npx -y @modelcontextprotocol/server-filesystem ~/Documents"),
        url: None,
        env: &[],
        headers: &[],
    },
    Preset {
        name: "github",
        label: "GitHub",
        detail: "Issues, pull requests and code search",
        command: None,
        url: Some("https://api.githubcopilot.com/mcp/"),
        env: &[],
        headers: &[("Authorization", "Bearer ")],
    },
    Preset {
        name: "playwright",
        label: "Playwright",
        detail: "Drive a real browser",
        command: Some("npx -y @playwright/mcp@latest"),
        url: None,
        env: &[],
        headers: &[],
    },
    Preset {
        name: "fetch",
        label: "Fetch",
        detail: "Read web pages as text",
        command: Some("uvx mcp-server-fetch"),
        url: None,
        env: &[],
        headers: &[],
    },
    Preset {
        name: "context7",
        label: "Context7",
        detail: "Up to date docs for libraries",
        command: None,
        url: Some("https://mcp.context7.com/mcp"),
        env: &[],
        headers: &[],
    },
];
