//! `sphinx.ext.githubpages`: `.nojekyll` and `CNAME` emission rules.

use scribe_config::Config;

/// Files the extension wants in the output directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GithubPagesFiles {
    /// Always `true` for HTML-format builders.
    pub nojekyll: bool,
    /// Hostname written to `CNAME`, when `html_baseurl` has a non-github.io host.
    pub cname: Option<String>,
}

impl GithubPagesFiles {
    /// Compute the files from configuration.
    #[must_use]
    pub fn from_config(config: &Config) -> Self {
        let baseurl = config.get_str("html_baseurl").unwrap_or("");
        let domain = domain_from_url(baseurl);
        let cname = match domain {
            Some(domain) if !domain.ends_with(".github.io") => Some(domain),
            _ => None,
        };
        Self { nojekyll: true, cname }
    }
}

/// `urllib.parse.urlparse(url).hostname` equivalent for the small subset of
/// URLs used by `html_baseurl`. Lower-cases the hostname as Python does.
#[must_use]
pub fn domain_from_url(url: &str) -> Option<String> {
    if url.is_empty() {
        return None;
    }
    let after_scheme = url.split_once("://").map_or(url, |(_, rest)| rest);
    let authority = after_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or("")
        .rsplit('@')
        .next()
        .unwrap_or("");
    let host = authority.split(':').next().unwrap_or("").trim().to_ascii_lowercase();
    if host.is_empty() { None } else { Some(host) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use scribe_config::ConfigValue;

    #[test]
    fn extracts_hostnames_like_urlparse() {
        assert_eq!(domain_from_url("https://example.com/docs").as_deref(), Some("example.com"));
        assert_eq!(domain_from_url("http://user:pw@Example.COM:8080/x").as_deref(), Some("example.com"));
        assert_eq!(domain_from_url(""), None);
        assert_eq!(domain_from_url("docs"), Some("docs".to_owned()));
    }

    #[test]
    fn cname_is_suppressed_for_github_io_domains() {
        let mut config = Config::new();
        config.set("html_baseurl", ConfigValue::from("https://user.github.io/docs")).unwrap();
        assert_eq!(GithubPagesFiles::from_config(&config).cname, None);
        config.set("html_baseurl", ConfigValue::from("https://docs.example.com")).unwrap();
        assert_eq!(GithubPagesFiles::from_config(&config).cname.as_deref(), Some("docs.example.com"));
    }
}
