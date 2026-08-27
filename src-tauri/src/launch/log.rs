//! Parsing the game's log output.
//!
//! When launched with a log4j configuration, Minecraft writes a stream of
//! `<log4j:Event>` elements rather than plain lines. Parsing them back gives
//! the console real levels, logger names and thread names instead of a wall of
//! undifferentiated text.

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogEntry {
    pub instance_id: String,
    pub level: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logger: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread: Option<String>,
}

impl LogEntry {
    pub fn stderr(instance_id: &str, message: String) -> Self {
        Self {
            instance_id: instance_id.to_string(),
            level: "ERROR".into(),
            message,
            logger: None,
            thread: Some("stderr".into()),
        }
    }

    pub fn with_instance(mut self, instance_id: &str) -> Self {
        self.instance_id = instance_id.to_string();
        self
    }
}

/// Incremental parser fed one line at a time.
///
/// Events span multiple lines and stack traces inside a `<![CDATA[...]]>` block
/// can contain almost anything, so the parser buffers until it sees the closing
/// tag rather than trying to match per line.
pub struct Log4jParser {
    buffer: String,
    in_event: bool,
}

impl Log4jParser {
    pub fn new() -> Self {
        Self {
            buffer: String::new(),
            in_event: false,
        }
    }

    /// Feed one line; returns any entries that completed.
    pub fn push(&mut self, line: &str) -> Vec<LogEntry> {
        let trimmed = line.trim_start();

        if !self.in_event {
            if trimmed.starts_with("<log4j:Event") {
                self.in_event = true;
                self.buffer.clear();
                self.buffer.push_str(line);
                self.buffer.push('\n');
                return self.try_complete();
            }
            // Output before log4j takes over, or from a version with no
            // logging config at all.
            if trimmed.is_empty() {
                return Vec::new();
            }
            return vec![plain(line)];
        }

        self.buffer.push_str(line);
        self.buffer.push('\n');
        self.try_complete()
    }

    fn try_complete(&mut self) -> Vec<LogEntry> {
        if !self.buffer.contains("</log4j:Event>") {
            // A malformed or truncated event would otherwise grow without
            // bound; cut our losses and fall back to plain lines.
            if self.buffer.len() > 512 * 1024 {
                let dropped = std::mem::take(&mut self.buffer);
                self.in_event = false;
                return dropped.lines().map(plain).collect();
            }
            return Vec::new();
        }

        let raw = std::mem::take(&mut self.buffer);
        self.in_event = false;
        match parse_event(&raw) {
            Some(entry) => vec![entry],
            None => raw.lines().map(plain).collect(),
        }
    }

    /// Emit whatever is left when the stream ends mid-event.
    pub fn flush(&mut self) -> Vec<LogEntry> {
        if self.buffer.is_empty() {
            return Vec::new();
        }
        let raw = std::mem::take(&mut self.buffer);
        self.in_event = false;
        raw.lines().filter(|l| !l.trim().is_empty()).map(plain).collect()
    }
}

impl Default for Log4jParser {
    fn default() -> Self {
        Self::new()
    }
}

fn plain(line: &str) -> LogEntry {
    // Best-effort level detection for unstructured output, so a plain-text
    // stack trace still shows up red.
    let level = if line.contains("ERROR") || line.contains("Exception") {
        "ERROR"
    } else if line.contains("WARN") {
        "WARN"
    } else {
        "INFO"
    };
    LogEntry {
        instance_id: String::new(),
        level: level.into(),
        message: line.trim_end().to_string(),
        logger: None,
        thread: None,
    }
}

fn parse_event(raw: &str) -> Option<LogEntry> {
    let level = attribute(raw, "level").unwrap_or_else(|| "INFO".into());
    let logger = attribute(raw, "logger");
    let thread = attribute(raw, "thread");

    let mut message = cdata_after(raw, "<log4j:Message>")?;
    // A `<log4j:Throwable>` sibling holds the stack trace for logged exceptions.
    if let Some(throwable) = cdata_after(raw, "<log4j:Throwable>") {
        if !throwable.trim().is_empty() {
            message.push('\n');
            message.push_str(throwable.trim_end());
        }
    }

    Some(LogEntry {
        instance_id: String::new(),
        level,
        message: message.trim_end().to_string(),
        logger,
        thread,
    })
}

/// Read `name="value"` from the opening tag.
fn attribute(raw: &str, name: &str) -> Option<String> {
    let needle = format!("{name}=\"");
    let start = raw.find(&needle)? + needle.len();
    let end = raw[start..].find('"')? + start;
    Some(unescape(&raw[start..end]))
}

/// Extract the CDATA payload that follows `tag`.
fn cdata_after(raw: &str, tag: &str) -> Option<String> {
    let tag_at = raw.find(tag)? + tag.len();
    let rest = &raw[tag_at..];
    let open = rest.find("<![CDATA[")? + "<![CDATA[".len();
    let close = rest[open..].find("]]>")? + open;
    Some(rest[open..close].to_string())
}

fn unescape(s: &str) -> String {
    s.replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_single_line_event() {
        let mut p = Log4jParser::new();
        let mut out = Vec::new();
        for line in [
            r#"<log4j:Event logger="net.minecraft.client.Main" timestamp="1" level="INFO" thread="Render thread">"#,
            r#"<log4j:Message><![CDATA[Setting user: Steve]]></log4j:Message>"#,
            r#"</log4j:Event>"#,
        ] {
            out.extend(p.push(line));
        }
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].level, "INFO");
        assert_eq!(out[0].message, "Setting user: Steve");
        assert_eq!(out[0].thread.as_deref(), Some("Render thread"));
        assert_eq!(out[0].logger.as_deref(), Some("net.minecraft.client.Main"));
    }

    #[test]
    fn appends_throwable_to_message() {
        let mut p = Log4jParser::new();
        let mut out = Vec::new();
        for line in [
            r#"<log4j:Event logger="x" timestamp="1" level="ERROR" thread="main">"#,
            r#"<log4j:Message><![CDATA[Boom]]></log4j:Message>"#,
            r#"<log4j:Throwable><![CDATA[java.lang.RuntimeException: nope"#,
            r#"	at x.y.Z.main(Z.java:1)]]></log4j:Throwable>"#,
            r#"</log4j:Event>"#,
        ] {
            out.extend(p.push(line));
        }
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].level, "ERROR");
        assert!(out[0].message.starts_with("Boom\njava.lang.RuntimeException"));
        assert!(out[0].message.contains("at x.y.Z.main"));
    }

    #[test]
    fn passes_through_plain_lines() {
        let mut p = Log4jParser::new();
        let out = p.push("Picked up JAVA_TOOL_OPTIONS: -Dfoo");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].message, "Picked up JAVA_TOOL_OPTIONS: -Dfoo");
    }

    #[test]
    fn flags_plain_exceptions_as_errors() {
        let mut p = Log4jParser::new();
        let out = p.push("java.lang.NullPointerException: oops");
        assert_eq!(out[0].level, "ERROR");
    }

    #[test]
    fn unescapes_attributes() {
        let mut p = Log4jParser::new();
        let mut out = Vec::new();
        for line in [
            r#"<log4j:Event logger="a&amp;b" timestamp="1" level="WARN" thread="t">"#,
            r#"<log4j:Message><![CDATA[hi]]></log4j:Message>"#,
            r#"</log4j:Event>"#,
        ] {
            out.extend(p.push(line));
        }
        assert_eq!(out[0].logger.as_deref(), Some("a&b"));
    }

    #[test]
    fn flush_recovers_truncated_event() {
        let mut p = Log4jParser::new();
        p.push(r#"<log4j:Event logger="x" level="INFO" thread="t">"#);
        let out = p.flush();
        assert_eq!(out.len(), 1);
    }
}
