//! Official Spring Boot 3.5.0 `launch.script` (Apache-2.0) plus
//! `DefaultLaunchScript` `{{name:default}}` expansion (no custom properties).
//!
//! Source:
//! https://github.com/spring-projects/spring-boot/blob/v3.5.0/spring-boot-project/spring-boot-tools/spring-boot-loader-tools/src/main/resources/org/springframework/boot/loader/tools/launch.script

use std::sync::OnceLock;

const TEMPLATE: &[u8] = include_bytes!("fixtures/spring-boot-3.5.0-launch.script");

/// Exact upstream bytes (placeholders still present). Must stay LF-only.
pub fn official_launch_script_template() -> &'static [u8] {
    TEMPLATE
}

/// Rendered as in a real `executable: true` / `bootJar { launchScript() }` build.
pub fn spring_boot_launch_script() -> &'static [u8] {
    static RENDERED: OnceLock<Vec<u8>> = OnceLock::new();
    RENDERED
        .get_or_init(|| render_spring_placeholders(TEMPLATE))
        .as_slice()
}

/// Java `\{\{(\w+)(:.*?)?}}(?!})`. Unset properties take the default after `:`.
fn render_spring_placeholders(src: &[u8]) -> Vec<u8> {
    let s = std::str::from_utf8(src).expect("launch.script is UTF-8");
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        if let Some((consumed, value)) = parse_placeholder(&s[i..]) {
            out.push_str(value);
            i += consumed;
        } else {
            let ch = s[i..].chars().next().unwrap();
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    out.into_bytes()
}

fn parse_placeholder(s: &str) -> Option<(usize, &str)> {
    let rest = s.strip_prefix("{{")?;
    let name_len = rest
        .bytes()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == b'_')
        .count();
    if name_len == 0 {
        return None;
    }
    let after_name = &rest[name_len..];
    if let Some(after_colon) = after_name.strip_prefix(':') {
        // Java `.` does not match newlines; `}}(?!})` stays on this line.
        let line_end = after_colon.find('\n').unwrap_or(after_colon.len());
        let line = &after_colon[..line_end];
        let mut search = 0;
        while let Some(rel) = line[search..].find("}}") {
            let abs = search + rel;
            let close_end = abs + 2;
            if line.get(close_end..close_end + 1) != Some("}") {
                let default = &line[..abs];
                return Some((2 + name_len + 1 + close_end, default));
            }
            search = abs + 1;
        }
        None
    } else if let Some(after_close) = after_name.strip_prefix("}}") {
        if after_close.starts_with('}') {
            return None;
        }
        let consumed = 2 + name_len + 2;
        Some((consumed, &s[..consumed]))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn official_template_is_spring_boot_3_5_0_launch_script() {
        let raw = official_launch_script_template();
        assert!(!raw.contains(&b'\r'), "fixture must be LF-only");
        assert_eq!(raw.len(), 9570);
        assert!(raw.starts_with(b"#!/bin/bash\n"));
        assert!(raw
            .windows(b"### BEGIN INIT INFO".len())
            .any(|w| w == b"### BEGIN INIT INFO"));
        assert!(raw.windows(b"chkconfig".len()).any(|w| w == b"chkconfig"));
        assert!(raw
            .windows(b":: Spring Boot Startup Script ::".len())
            .any(|w| w == b":: Spring Boot Startup Script ::"));
        assert!(raw.ends_with(b"exit 0\n"));
        assert!(!raw.windows(4).any(|w| w == b"PK\x03\x04"));
    }

    #[test]
    fn rendered_script_fills_defaults_and_keeps_shape() {
        let rendered = spring_boot_launch_script();
        assert!(!rendered.contains(&b'\r'), "fixture must be LF-only");
        assert!(rendered.starts_with(b"#!/bin/bash\n"));
        assert!(rendered.ends_with(b"exit 0\n"));
        let s = std::str::from_utf8(rendered).unwrap();
        assert!(!s.contains("{{"), "placeholders must be expanded: {s}");
        assert!(s.contains("MODE=\"auto\""));
        assert!(s.contains("2345 99 01"));
        assert!(s.contains("chkconfig:"));
        assert!(!rendered.windows(4).any(|w| w == b"PK\x03\x04"));
        assert!(rendered.len() > 200);
    }
}
