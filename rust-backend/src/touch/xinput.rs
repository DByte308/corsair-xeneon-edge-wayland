// SPDX-License-Identifier: GPL-3.0-or-later
//! Pure parsing of `xinput list --short` output, ported from
//! agent/src/core/XinputParse.cpp.
//!
//! Extracted so the decision that matters can be tested. Creating a master
//! pointer on X11 also creates a master keyboard, and both come with their own
//! XTEST slave devices. Counting those XTEST slaves as "someone is using this
//! master" meant the master could never be removed, so every use of the
//! Own-pointer touch mode permanently left a second master keyboard behind, and
//! a second master keyboard splits keyboard focus: typing goes to whatever the
//! other pointer last focused.

/// Split a command capture into lines, mirroring `istringstream` + getline (the
/// trailing newline of a captured output does not produce an extra element).
pub fn split_lines(text: &str) -> Vec<String> {
    text.lines().map(|s| s.to_string()).collect()
}

pub(crate) fn id_in_line(line: &str) -> Option<i32> {
    let bytes = line.as_bytes();
    let n = bytes.len();
    let mut i = 0;
    while i + 2 < n {
        if bytes[i] == b'i' && bytes[i + 1] == b'd' && bytes[i + 2] == b'=' {
            let mut j = i + 3;
            let mut value = 0i64;
            while j < n && bytes[j].is_ascii_digit() {
                value = value * 10 + (bytes[j] - b'0') as i64;
                j += 1;
            }
            if j > i + 3 {
                return Some(value as i32);
            }
        }
        i += 1;
    }
    None
}

/// Id of the master pointer whose name contains `name`, or -1.
pub fn find_master_pointer_id(lines: &[String], name: &str) -> i32 {
    for line in lines {
        if !line.contains(name) {
            continue;
        }
        if !line.contains("master pointer") {
            continue;
        }
        if let Some(id) = id_in_line(line) {
            return id;
        }
    }
    -1
}

/// True when a master with this id still has a real slave attached, ignoring
/// the XTEST devices X creates alongside every master.
pub fn master_has_real_slaves(lines: &[String], master_id: i32) -> bool {
    if master_id < 0 {
        return false;
    }
    let tag = format!("({master_id})");
    for line in lines {
        if !line.contains("slave") {
            continue;
        }
        if !line.contains(&tag) {
            continue;
        }
        // X creates an XTEST pointer and keyboard for every master. They are
        // not a reason to keep one alive.
        if line.contains("XTEST") {
            continue;
        }
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    // Captured from the machine while the own-pointer-master fault was present:
    // the xeneon-edge master still exists but has no real slaves of its own.
    const ORPHANED_MASTER: &str = r#"
⎡ Virtual core pointer                    	id=2	[master pointer  (3)]
⎜   ↳ Virtual core XTEST pointer              	id=4	[slave  pointer  (2)]
⎜   ↳ Logitech USB Receiver Mouse             	id=10	[slave  pointer  (2)]
⎣ Virtual core keyboard                   	id=3	[master keyboard (2)]
    ↳ Virtual core XTEST keyboard             	id=5	[slave  keyboard (3)]
⎡ xeneon-edge pointer                     	id=23	[master pointer  (24)]
⎜   ↳ xeneon-edge XTEST pointer               	id=25	[slave  pointer  (23)]
⎣ xeneon-edge keyboard                    	id=24	[master keyboard (23)]
    ↳ xeneon-edge XTEST keyboard              	id=26	[slave  keyboard (24)]
∼ wch.cn TouchScreen                      	id=22	[floating slave]
"#;

    // The same tree while Own-pointer mode is genuinely in use.
    const MASTER_IN_USE: &str = r#"
⎡ Virtual core pointer                    	id=2	[master pointer  (3)]
⎣ Virtual core keyboard                   	id=3	[master keyboard (2)]
⎡ xeneon-edge pointer                     	id=23	[master pointer  (24)]
⎜   ↳ xeneon-edge XTEST pointer               	id=25	[slave  pointer  (23)]
⎜   ↳ wch.cn TouchScreen                      	id=22	[slave  pointer  (23)]
⎣ xeneon-edge keyboard                    	id=24	[master keyboard (23)]
    ↳ xeneon-edge XTEST keyboard              	id=26	[slave  keyboard (24)]
"#;

    const NO_EDGE_MASTER: &str = r#"
⎡ Virtual core pointer                    	id=2	[master pointer  (3)]
⎜   ↳ wch.cn TouchScreen                      	id=22	[slave  pointer  (2)]
⎣ Virtual core keyboard                   	id=3	[master keyboard (2)]
"#;

    #[test]
    fn orphaned_master_has_no_real_slaves() {
        let lines = split_lines(ORPHANED_MASTER);
        let id = find_master_pointer_id(&lines, "xeneon-edge");
        assert_eq!(id, 23);
        assert!(!master_has_real_slaves(&lines, id));
    }

    #[test]
    fn in_use_master_is_kept() {
        let lines = split_lines(MASTER_IN_USE);
        let id = find_master_pointer_id(&lines, "xeneon-edge");
        assert_eq!(id, 23);
        assert!(master_has_real_slaves(&lines, id));
    }

    #[test]
    fn missing_master_finds_nothing() {
        let lines = split_lines(NO_EDGE_MASTER);
        assert_eq!(find_master_pointer_id(&lines, "xeneon-edge"), -1);
        assert!(!master_has_real_slaves(&lines, -1));
    }

    #[test]
    fn core_master_is_never_ours() {
        let lines = split_lines(ORPHANED_MASTER);
        assert!(master_has_real_slaves(&lines, 2)); // the mouse is a real slave
        assert_eq!(find_master_pointer_id(&lines, "Virtual core"), 2);
    }

    #[test]
    fn keyboard_master_id_is_not_confused() {
        let lines = split_lines(ORPHANED_MASTER);
        // The pointer master is 23, not the keyboard master 24; removing the
        // pointer removes the pair, so 23 is the id to find.
        assert_ne!(find_master_pointer_id(&lines, "xeneon-edge"), 24);
    }

    #[test]
    fn ids_that_merely_share_digits_do_not_match() {
        let lines = split_lines(ORPHANED_MASTER);
        // "(2)" is not "(23)".
        assert!(!master_has_real_slaves(&lines, 25));
    }

    #[test]
    fn split_line_edge_cases() {
        assert!(split_lines("").is_empty());
        assert_eq!(split_lines("one\ntwo\n").len(), 2);
    }
}
