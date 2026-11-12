// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

pub fn hms() -> String {
    jiff::Zoned::now().strftime("%H:%M:%S").to_string()
}

#[cfg(test)]
mod tests {
    #[test]
    fn hms_is_eight_characters() {
        let s = super::hms();
        assert_eq!(s.len(), 8, "{s}");
        assert_eq!(s.as_bytes()[2], b':');
        assert_eq!(s.as_bytes()[5], b':');
    }
}
