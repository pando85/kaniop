// Parse semantic version string (e.g. "1.2.3") into (major, minor, patch)
pub fn parse_semver(tag: &str) -> Option<(u64, u64, u64)> {
    let parts: Vec<_> = tag.split('.').collect();
    if parts.len() >= 3 {
        let major = parts[0].parse().ok()?;
        let minor = parts[1].parse().ok()?;
        let patch = parts[2]
            .split(|c: char| !c.is_ascii_digit())
            .next()?
            .parse()
            .ok()?;
        Some((major, minor, patch))
    } else {
        None
    }
}
