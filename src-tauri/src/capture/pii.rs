use regex::Regex;

/// Replace PII patterns in text with `***`.
pub fn sanitize(text: &str) -> String {
    // Credit card: 4 groups of 4 digits separated by spaces or dashes
    let cc = Regex::new(r"\b\d{4}[- ]\d{4}[- ]\d{4}[- ]\d{4}\b").unwrap();
    let text = cc.replace_all(text, "***");

    // SSN: 123-45-6789
    let ssn = Regex::new(r"\b\d{3}-\d{2}-\d{4}\b").unwrap();
    let text = ssn.replace_all(&text, "***");

    // Chinese ID: 18 digits or 17 digits + X
    let cn_id = Regex::new(r"\b\d{17}[\dX]\b").unwrap();
    let text = cn_id.replace_all(&text, "***");

    // Chinese phone: 11 digits starting with 1[3-9]
    let cn_phone = Regex::new(r"\b1[3-9]\d{9}\b").unwrap();
    let text = cn_phone.replace_all(&text, "***");

    // Email
    let email = Regex::new(r"\w+@\w+\.\w+").unwrap();
    let text = email.replace_all(&text, "***");

    text.into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_credit_card() {
        assert_eq!(sanitize("card 1234 5678 9012 3456 ok"), "card *** ok");
        assert_eq!(sanitize("card 1234-5678-9012-3456 ok"), "card *** ok");
    }

    #[test]
    fn test_ssn() {
        assert_eq!(sanitize("ssn 123-45-6789 end"), "ssn *** end");
    }

    #[test]
    fn test_chinese_id() {
        assert_eq!(sanitize("id 11010119900307891X end"), "id *** end");
        assert_eq!(sanitize("id 110101199003078910 end"), "id *** end");
    }

    #[test]
    fn test_chinese_phone() {
        assert_eq!(sanitize("call 13812345678 now"), "call *** now");
    }

    #[test]
    fn test_email() {
        assert_eq!(sanitize("mail user@example.com ok"), "mail *** ok");
    }

    #[test]
    fn test_no_pii() {
        assert_eq!(sanitize("hello world"), "hello world");
    }
}
