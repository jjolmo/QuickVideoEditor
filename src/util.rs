use glib::warn;

use crate::config::G_LOG_DOMAIN;

fn freplace(s: &str, args: &[&str]) -> String {
    let mut parts = s.split("{}").peekable();
    let mut rv = parts.next().unwrap().to_owned();
    let mut args = args.iter().peekable();

    // Handle the normal case: matching args count.
    while let Some((arg, part)) = args.peek().zip(parts.peek()) {
        rv.push_str(arg);
        rv.push_str(part);
        args.next();
        parts.next();
    }

    // Check if there are more args than necessary.
    let args_left = args.count();
    if args_left > 0 {
        warn!("translation `{s}` is missing {args_left} placeholder(-s)");
    }

    // Handle the case when there are not enough args.
    let mut extra_parts = 0;
    for part in parts {
        rv.push_str("{}");
        rv.push_str(part);
        extra_parts += 1;
    }

    if extra_parts > 0 {
        warn!("translation `{s}` has {extra_parts} extra placeholder(-s)");
    }

    rv
}

pub fn gettext_f(format: &str, args: &[&str]) -> String {
    let s = gettextrs::gettext(format);
    freplace(&s, args)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn freplace_works() {
        assert_eq!(freplace("asdf", &[]), "asdf");
        assert_eq!(freplace("hello {} world", &["nice"]), "hello nice world");
        assert_eq!(
            freplace("{}hello {} world{}", &["very", "nice", "hi"]),
            "veryhello nice worldhi"
        );
    }

    #[test]
    fn freplace_not_enough_args() {
        assert_eq!(
            freplace("first {} second {} third {} fourth", &["one"]),
            "first one second {} third {} fourth"
        );
    }

    #[test]
    fn freplace_too_many_args() {
        assert_eq!(
            freplace(
                "first {} second {} third {} fourth",
                &["one", "two", "three", "four", "five", "six"]
            ),
            "first one second two third three fourth"
        );
    }
}
