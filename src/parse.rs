use std::str::FromStr;

use nom::{
    branch::alt,
    bytes::complete::take_while_m_n,
    character::complete::char,
    combinator::{all_consuming, map, map_res, opt, verify},
    sequence::{pair, preceded, separated_pair, terminated, tuple},
    IResult,
};

fn is_digit(input: char) -> bool {
    input.is_digit(10)
}

fn digits_m_n(m: usize, n: usize, input: &str) -> IResult<&str, u32> {
    map_res(take_while_m_n(m, n, is_digit), u32::from_str)(input)
}

fn one_digit(input: &str) -> IResult<&str, u32> {
    digits_m_n(1, 1, input)
}

fn one_or_two_digits(input: &str) -> IResult<&str, u32> {
    digits_m_n(1, 2, input)
}

fn two_digits(input: &str) -> IResult<&str, u32> {
    digits_m_n(2, 2, input)
}

fn milliseconds(input: &str) -> IResult<&str, u32> {
    map(
        tuple((one_digit, opt(one_digit), opt(one_digit))),
        |(h, t, u)| h * 100 + t.unwrap_or(0) * 10 + u.unwrap_or(0),
    )(input)
}

pub fn timestamp(input: &str) -> Option<u32> {
    let hours_minutes_seconds = tuple((
        terminated(one_or_two_digits, char(':')),
        terminated(two_digits, char(':')),
        two_digits,
    ));
    let minutes_seconds = map(
        separated_pair(one_or_two_digits, char(':'), two_digits),
        |(minutes, seconds)| (0, minutes, seconds),
    );
    let seconds = map(one_or_two_digits, |seconds| (0, 0, seconds));

    let non_fractional = verify(
        alt((hours_minutes_seconds, minutes_seconds, seconds)),
        |&(_hours, minutes, seconds)| minutes < 60 && seconds < 60,
    );

    let fractional = preceded(char('.'), milliseconds);

    let parser = map(
        pair(non_fractional, opt(fractional)),
        |((hours, minutes, seconds), milliseconds)| {
            hours * 3_600_000 + minutes * 60_000 + seconds * 1000 + milliseconds.unwrap_or(0)
        },
    );

    all_consuming(parser)(input).ok().map(|(_i, o)| o)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamp_works() {
        assert_eq!(timestamp("1"), Some(1000));
        assert_eq!(timestamp("1:23:45.678"), Some(5025678));
        assert_eq!(timestamp("2.43"), Some(2430));
        assert_eq!(timestamp("1."), None);
        assert_eq!(timestamp("60"), None);
        assert_eq!(timestamp(":3"), None);
        assert_eq!(timestamp("2:"), None);
        assert_eq!(timestamp("2:03"), Some(123000));
    }
}
