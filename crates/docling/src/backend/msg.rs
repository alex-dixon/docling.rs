//! Outlook `.msg` → RFC 822 projection (#251, docling#3873).
//!
//! A `.msg` is a CFB (OLE2) container of MAPI property streams. Instead of
//! emitting document nodes directly, the message is projected onto RFC 822
//! bytes and fed through the same mail-parser path as `.eml` input — exactly
//! docling's approach (python-oxmsg → `EmailMessage` → mailparser), which
//! makes `.msg` and `.eml` output identical by construction.
//!
//! Property streams are named `__substg1.0_TTTTYYYY` (TTTT = MAPI property
//! id, YYYY = type: `001F` UTF-16LE, `001E` 8-bit, `0102` binary); fixed-size
//! properties (dates, longs) live in `__properties_version1.0`. Recipients
//! and attachments are `__recip_version1.0_#N` / `__attach_version1.0_#N`
//! sub-storages repeating the same stream names, which is why the CFB walk
//! is storage-aware. Layouts per [MS-OXMSG].

use crate::backend::cfb::CompoundFile;

/// The parsed projection: RFC 822 bytes for the shared mail-parser path plus
/// the attachment display labels (`name (mime/type)`) for `list_attachments`
/// — surfaced from MAPI directly, so no MIME multipart needs synthesizing.
pub(crate) struct ProjectedMsg {
    pub(crate) rfc822: Vec<u8>,
    pub(crate) attachment_labels: Vec<String>,
}

/// Project `.msg` bytes onto RFC 822. `None` when the container doesn't
/// parse as CFB at all; a parse with missing properties degrades to absent
/// headers rather than failing (a bare subject-less note still converts).
pub(crate) fn project(data: &[u8]) -> Option<ProjectedMsg> {
    let cfb = CompoundFile::open(data)?;
    Some(project_entries(&cfb, &cfb.children_of(None)))
}

/// Project the message whose property streams are `root` — the file's root
/// storage, or an embedded message's `__substg1.0_3701000D` storage (#561:
/// a forwarded `.msg` inside a `.msg`, whose RFC 822 projection is then its
/// attachment payload).
pub(crate) fn project_entries(cfb: &CompoundFile, root: &[usize]) -> ProjectedMsg {
    let prop = |id: &str| -> Option<String> { read_string(cfb, root, id) };

    let subject = prop("0037");
    // Sender: PR_SENDER_* first, PR_SENT_REPRESENTING_* as the fallback
    // (python-oxmsg's Message.sender resolution).
    let from = address(
        prop("0C1A").or_else(|| prop("0042")),
        prop("0C1F").or_else(|| prop("0065")),
    );

    // Recipients: each `__recip_version1.0_#N` storage carries a display name
    // (3001), an SMTP address (39FE, falling back to the address-type 3003),
    // and PR_RECIPIENT_TYPE (0C15: 1 = To, 2 = Cc) in its fixed properties.
    let mut to: Vec<String> = Vec::new();
    let mut cc: Vec<String> = Vec::new();
    for idx in root.iter().copied() {
        if !cfb.is_storage(idx) || !cfb.entry_name(idx).starts_with("__recip_version1.0_#") {
            continue;
        }
        let kids = cfb.children_of(Some(idx));
        let addr = address(
            read_string(cfb, &kids, "3001"),
            read_string(cfb, &kids, "39FE").or_else(|| read_string(cfb, &kids, "3003")),
        );
        let Some(addr) = addr else { continue };
        match fixed_u32(cfb, &kids, 0x0C15).unwrap_or(1) {
            2 => cc.push(addr),
            3 => {} // Bcc is not a header docling surfaces
            _ => to.push(addr),
        }
    }

    // PR_CLIENT_SUBMIT_TIME (0039), falling back to the delivery time (0E06):
    // a FILETIME in the root fixed-property stream.
    let date = fixed_filetime(cfb, root, 0x0039)
        .or_else(|| fixed_filetime(cfb, root, 0x0E06))
        .map(rfc2822_utc);

    // Plain-text body (1000). An RTF-only message (compressed 10090102, no
    // plain body) degrades to an empty body rather than failing — the
    // headers still convert. (LZFu decompression can come on demand.)
    let body = read_string(cfb, root, "1000").unwrap_or_default();

    let mut labels: Vec<String> = Vec::new();
    for idx in attachment_storages(cfb, root) {
        let kids = cfb.children_of(Some(idx));
        // Long filename (3707) over the 8.3 one (3704); the MIME tag (370E)
        // parenthesizes when present — docling's attachment label format.
        let name = attachment_name(cfb, &kids)
            .unwrap_or_else(|| format!("attachment-{}", labels.len() + 1));
        let label = match attachment_mime(cfb, &kids) {
            Some(mime) => format!("{name} ({mime})"),
            None => name,
        };
        labels.push(label);
    }

    let mut out = String::new();
    if let Some(f) = from {
        out.push_str(&format!("From: {f}\r\n"));
    }
    if !to.is_empty() {
        out.push_str(&format!("To: {}\r\n", to.join(", ")));
    }
    if !cc.is_empty() {
        out.push_str(&format!("Cc: {}\r\n", cc.join(", ")));
    }
    if let Some(s) = subject {
        out.push_str(&format!("Subject: {s}\r\n"));
    }
    if let Some(d) = date {
        out.push_str(&format!("Date: {d}\r\n"));
    }
    out.push_str("MIME-Version: 1.0\r\nContent-Type: text/plain; charset=\"utf-8\"\r\n\r\n");
    out.push_str(&body);

    ProjectedMsg {
        rfc822: out.into_bytes(),
        attachment_labels: labels,
    }
}

/// The `__attach_version1.0_#N` storages under `root`, in directory order.
fn attachment_storages(cfb: &CompoundFile, root: &[usize]) -> Vec<usize> {
    root.iter()
        .copied()
        .filter(|&idx| {
            cfb.is_storage(idx) && cfb.entry_name(idx).starts_with("__attach_version1.0_#")
        })
        .collect()
}

/// An attachment's file name: the long name (3707) over the 8.3 one (3704),
/// then the display name (3001) an embedded message carries instead.
fn attachment_name(cfb: &CompoundFile, kids: &[usize]) -> Option<String> {
    ["3707", "3704", "3001"]
        .iter()
        .find_map(|id| read_string(cfb, kids, id))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// PR_ATTACH_MIME_TAG (370E), trimmed; `None` when absent or blank.
fn attachment_mime(cfb: &CompoundFile, kids: &[usize]) -> Option<String> {
    read_string(cfb, kids, "370E")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// One `.msg` attachment with its payload (#561), as MAPI stores it.
pub(crate) struct MsgAttachment {
    pub(crate) name: Option<String>,
    pub(crate) mime: Option<String>,
    /// PR_ATTACH_METHOD (3705): 1 by value, 2–4 and 7 by reference, 5 an
    /// embedded message, 6 an OLE object. Missing reads as by value, which
    /// is what writers that omit it mean.
    pub(crate) method: u32,
    /// The bytes: PR_ATTACH_DATA_BIN (37010102) for a by-value attachment,
    /// the RFC 822 projection of the `__substg1.0_3701000D` storage for an
    /// embedded message; `None` for a reference, an OLE object or a
    /// by-value attachment whose data stream is missing.
    pub(crate) payload: Option<Vec<u8>>,
    /// PR_ATTACHMENT_HIDDEN (7FFE) — an inline image of the HTML body — or
    /// `ATT_MHTML_REF` in PR_ATTACH_FLAGS (3714), or a content id (3712).
    pub(crate) inline: bool,
}

/// The attachments of `.msg` bytes with their payloads. `None` when the
/// container doesn't parse as CFB.
pub(crate) fn attachments(data: &[u8]) -> Option<Vec<MsgAttachment>> {
    let cfb = CompoundFile::open(data)?;
    let root = cfb.children_of(None);
    let mut out = Vec::new();
    for idx in attachment_storages(&cfb, &root) {
        let kids = cfb.children_of(Some(idx));
        let method = fixed_u32(&cfb, &kids, 0x3705).unwrap_or(1);
        let (payload, mime, name) = match method {
            // An embedded message is a storage of property streams, not a
            // byte string: project it like the outer message, one level —
            // its own attachments stay inside its RFC 822 as labels only.
            5 => {
                let storage = kids
                    .iter()
                    .copied()
                    .find(|&k| cfb.is_storage(k) && cfb.entry_name(k) == "__substg1.0_3701000D");
                let projected = storage.map(|s| project_entries(&cfb, &cfb.children_of(Some(s))));
                (
                    projected.map(|p| p.rfc822),
                    Some("message/rfc822".to_string()),
                    attachment_name(&cfb, &kids).map(|n| format!("{n}.eml")),
                )
            }
            1 => {
                let data = kids
                    .iter()
                    .find(|&&k| cfb.entry_name(k) == "__substg1.0_37010102")
                    .and_then(|&k| cfb.stream_by_index(k));
                (
                    data,
                    attachment_mime(&cfb, &kids),
                    attachment_name(&cfb, &kids),
                )
            }
            _ => (
                None,
                attachment_mime(&cfb, &kids),
                attachment_name(&cfb, &kids),
            ),
        };
        let hidden = fixed_u32(&cfb, &kids, 0x7FFE).is_some_and(|v| v & 0xFF != 0);
        let mhtml_ref = fixed_u32(&cfb, &kids, 0x3714).is_some_and(|f| f & 4 != 0);
        let content_id = read_string(&cfb, &kids, "3712").is_some_and(|s| !s.trim().is_empty());
        out.push(MsgAttachment {
            name,
            mime,
            method,
            payload,
            inline: hidden || mhtml_ref || content_id,
        });
    }
    Some(out)
}

/// `"Name <email>"`, bare email, or bare name — whatever the properties give.
fn address(name: Option<String>, email: Option<String>) -> Option<String> {
    let name = name.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let email = email
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    match (name, email) {
        (Some(n), Some(e)) => Some(format!("{n} <{e}>")),
        (None, Some(e)) => Some(e),
        (Some(n), None) => Some(n),
        (None, None) => None,
    }
}

/// A string property from `entries` (a storage's children): the UTF-16LE
/// variant (`001F`) first, then the 8-bit one (`001E`, decoded as cp1252 like
/// the rest of the legacy-format backends).
fn read_string(cfb: &CompoundFile, entries: &[usize], id: &str) -> Option<String> {
    let find = |suffix: &str| -> Option<Vec<u8>> {
        let want = format!("__substg1.0_{id}{suffix}");
        entries
            .iter()
            .find(|&&i| cfb.entry_name(i) == want)
            .and_then(|&i| cfb.stream_by_index(i))
    };
    if let Some(b) = find("001F") {
        let s: String = b
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .map(|u| char::from_u32(u as u32).unwrap_or('\u{FFFD}'))
            .collect();
        return Some(s.trim_end_matches('\0').to_string());
    }
    if let Some(b) = find("001E") {
        let s: String = b.iter().map(|&x| super::doc::cp1252(x)).collect();
        return Some(s.trim_end_matches('\0').to_string());
    }
    None
}

/// A fixed-size property's raw 8-byte value from `__properties_version1.0`.
/// The stream is a header (32 bytes at the root storage, 24 in an embedded
/// message's, 8 in recipient/attachment storages) followed by 16-byte
/// records: u16 type, u16 id, u32 flags, 8-byte value.
fn fixed_raw(cfb: &CompoundFile, entries: &[usize], id: u16) -> Option<[u8; 8]> {
    let stream = entries
        .iter()
        .find(|&&i| cfb.entry_name(i) == "__properties_version1.0")
        .and_then(|&i| cfb.stream_by_index(i))?;
    // The header length differs by storage kind; scanning from both offsets
    // is simpler than tracking which storage we're in, and a misaligned scan
    // can't match a real (type, id) pair by accident.
    for head in [32usize, 24, 8] {
        let Some(body) = stream.get(head..) else {
            continue;
        };
        for rec in body.chunks_exact(16) {
            let rid = u16::from_le_bytes([rec[2], rec[3]]);
            if rid == id {
                return rec[8..16].try_into().ok();
            }
        }
    }
    None
}

fn fixed_u32(cfb: &CompoundFile, entries: &[usize], id: u16) -> Option<u32> {
    fixed_raw(cfb, entries, id).map(|v| u32::from_le_bytes([v[0], v[1], v[2], v[3]]))
}

/// A FILETIME property (100 ns ticks since 1601-01-01 UTC) as Unix seconds.
fn fixed_filetime(cfb: &CompoundFile, entries: &[usize], id: u16) -> Option<i64> {
    let ticks = fixed_raw(cfb, entries, id).map(u64::from_le_bytes)?;
    if ticks == 0 {
        return None;
    }
    Some((ticks / 10_000_000) as i64 - 11_644_473_600)
}

/// Unix seconds → an RFC 2822 date header (`Tue, 20 May 2026 10:30:00 +0000`).
/// Hand-rolled civil-from-days (Howard Hinnant's algorithm) — the converter
/// carries no date-time dependency.
fn rfc2822_utc(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400);
    let (h, m, s) = (tod / 3600, (tod / 60) % 60, tod % 60);
    // 1970-01-01 was a Thursday.
    let weekday = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"][days.rem_euclid(7) as usize];
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { y + 1 } else { y };
    let month_name = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ][(month - 1) as usize];
    format!("{weekday}, {day} {month_name} {year} {h:02}:{m:02}:{s:02} +0000")
}

#[cfg(test)]
mod tests {
    #[test]
    fn civil_conversion_matches_known_dates() {
        // 2026-05-20 10:30:00 UTC (the .msg fixtures' PR_CLIENT_SUBMIT_TIME)
        // and the epoch edge.
        assert_eq!(
            super::rfc2822_utc(1_779_273_000),
            "Wed, 20 May 2026 10:30:00 +0000"
        );
        assert_eq!(super::rfc2822_utc(0), "Thu, 1 Jan 1970 00:00:00 +0000");
    }
}
