//! Exact orientation of three binary64 positions.
//!
//! A floating-point filter decides almost every case. Inputs that the filter
//! cannot classify are evaluated exactly as integers scaled by 2^1074, which
//! represents every finite binary64 value without rounding or underflow.
use std::cmp::Ordering;

pub(crate) type Position = (f64, f64);

const ERROR_BOUND: f64 = (3.0 + 16.0 * f64::EPSILON / 2.0) * f64::EPSILON / 2.0;
const MIN_FILTERED: f64 = 1e-280;

/// Sign of the cross product (b - a) x (c - a): Greater when c lies to the
/// left of the directed line from a to b, Less when to the right.
pub(crate) fn orientation(a: Position, b: Position, c: Position) -> Ordering {
    if c == a || c == b || a == b {
        return Ordering::Equal;
    }
    let left = (b.0 - a.0) * (c.1 - a.1);
    let right = (b.1 - a.1) * (c.0 - a.0);
    let det = left - right;
    let sum = left.abs() + right.abs();
    if sum >= MIN_FILTERED && det.abs() > ERROR_BOUND * sum {
        return if det > 0.0 {
            Ordering::Greater
        } else {
            Ordering::Less
        };
    }
    exact_orientation(a, b, c)
}

fn exact_orientation(a: Position, b: Position, c: Position) -> Ordering {
    let [ax, ay, bx, by, cx, cy] = [a.0, a.1, b.0, b.1, c.0, c.1].map(Int::from_f64);
    let left = bx.sub(&ax).mul(&cy.sub(&ay));
    let right = by.sub(&ay).mul(&cx.sub(&ax));
    left.compare(&right)
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Int {
    negative: bool,
    magnitude: Vec<u32>,
}

impl Int {
    fn from_f64(value: f64) -> Self {
        assert!(value.is_finite(), "orientation requires finite coordinates");
        let bits = value.to_bits();
        let exponent = ((bits >> 52) & 0x7ff) as usize;
        let fraction = bits & ((1u64 << 52) - 1);
        let (mantissa, shift) = if exponent == 0 {
            (fraction, 0)
        } else {
            (fraction | (1u64 << 52), exponent - 1)
        };
        let mut magnitude = vec![0u32; shift / 32];
        let wide = (mantissa as u128) << (shift % 32);
        let mut rest = wide;
        while rest != 0 {
            magnitude.push(rest as u32);
            rest >>= 32;
        }
        Self::normalized(value.is_sign_negative(), magnitude)
    }
    fn normalized(negative: bool, mut magnitude: Vec<u32>) -> Self {
        while magnitude.last() == Some(&0) {
            magnitude.pop();
        }
        let negative = negative && !magnitude.is_empty();
        Self {
            negative,
            magnitude,
        }
    }
    fn sub(&self, other: &Self) -> Self {
        let negated = Self::normalized(!other.negative, other.magnitude.clone());
        self.add(&negated)
    }
    fn add(&self, other: &Self) -> Self {
        if self.negative == other.negative {
            return Self::normalized(
                self.negative,
                add_magnitudes(&self.magnitude, &other.magnitude),
            );
        }
        match compare_magnitudes(&self.magnitude, &other.magnitude) {
            Ordering::Less => Self::normalized(
                other.negative,
                sub_magnitudes(&other.magnitude, &self.magnitude),
            ),
            _ => Self::normalized(
                self.negative,
                sub_magnitudes(&self.magnitude, &other.magnitude),
            ),
        }
    }
    fn mul(&self, other: &Self) -> Self {
        let mut product = vec![0u32; self.magnitude.len() + other.magnitude.len()];
        for (i, &x) in self.magnitude.iter().enumerate() {
            let mut carry = 0u64;
            for (j, &y) in other.magnitude.iter().enumerate() {
                let total = product[i + j] as u64 + x as u64 * y as u64 + carry;
                product[i + j] = total as u32;
                carry = total >> 32;
            }
            let mut k = i + other.magnitude.len();
            while carry != 0 {
                let total = product[k] as u64 + carry;
                product[k] = total as u32;
                carry = total >> 32;
                k += 1;
            }
        }
        Self::normalized(self.negative != other.negative, product)
    }
    fn compare(&self, other: &Self) -> Ordering {
        match (self.negative, other.negative) {
            (false, true) => Ordering::Greater,
            (true, false) => Ordering::Less,
            (false, false) => compare_magnitudes(&self.magnitude, &other.magnitude),
            (true, true) => compare_magnitudes(&other.magnitude, &self.magnitude),
        }
    }
}

fn add_magnitudes(a: &[u32], b: &[u32]) -> Vec<u32> {
    let mut sum = Vec::with_capacity(a.len().max(b.len()) + 1);
    let mut carry = 0u64;
    for i in 0..a.len().max(b.len()) {
        let total = *a.get(i).unwrap_or(&0) as u64 + *b.get(i).unwrap_or(&0) as u64 + carry;
        sum.push(total as u32);
        carry = total >> 32;
    }
    if carry != 0 {
        sum.push(carry as u32);
    }
    sum
}

fn sub_magnitudes(larger: &[u32], smaller: &[u32]) -> Vec<u32> {
    let mut difference = Vec::with_capacity(larger.len());
    let mut borrow = 0i64;
    for (i, &x) in larger.iter().enumerate() {
        let mut total = x as i64 - *smaller.get(i).unwrap_or(&0) as i64 - borrow;
        borrow = 0;
        if total < 0 {
            total += 1i64 << 32;
            borrow = 1;
        }
        difference.push(total as u32);
    }
    assert_eq!(
        borrow, 0,
        "magnitude subtraction requires the larger value first"
    );
    difference
}

fn compare_magnitudes(a: &[u32], b: &[u32]) -> Ordering {
    a.len()
        .cmp(&b.len())
        .then_with(|| a.iter().rev().cmp(b.iter().rev()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filtered_and_exact_orientation_agree_on_clear_cases() {
        let cases = [
            ((0.0, 0.0), (1.0, 0.0), (0.0, 1.0), Ordering::Greater),
            ((0.0, 0.0), (1.0, 0.0), (0.0, -1.0), Ordering::Less),
            ((0.0, 0.0), (1.0, 1.0), (2.0, 2.0), Ordering::Equal),
            ((-180.0, -89.5), (180.0, 89.5), (0.0, 0.0), Ordering::Equal),
        ];
        for (a, b, c, expected) in cases {
            assert_eq!(orientation(a, b, c), expected);
            assert_eq!(exact_orientation(a, b, c), expected);
        }
    }

    #[test]
    fn exact_orientation_resolves_points_beyond_float_resolution() {
        let a = (0.5, 0.5);
        let b = (12.0, 12.0);
        let base = 0.5f64;
        for steps in 1..64u64 {
            let x = f64::from_bits(base.to_bits() + steps);
            let c = (x, 0.5);
            let expected = Ordering::Less;
            assert_eq!(orientation(a, b, c), expected, "step {steps}");
            let d = (0.5, x);
            assert_eq!(orientation(a, b, d), Ordering::Greater, "step {steps}");
        }
        let on_line = (24.0, 24.0);
        assert_eq!(orientation(a, b, on_line), Ordering::Equal);
    }

    #[test]
    fn exact_orientation_handles_subnormal_and_signed_zero_inputs() {
        let tiny = f64::from_bits(1);
        assert_eq!(
            orientation((0.0, 0.0), (tiny, 0.0), (0.0, tiny)),
            Ordering::Greater
        );
        assert_eq!(
            orientation((-0.0, 0.0), (tiny, 0.0), (0.0, -tiny)),
            Ordering::Less
        );
        assert_eq!(
            orientation((0.0, -0.0), (tiny, tiny), (2.0 * tiny, 2.0 * tiny)),
            Ordering::Equal
        );
        assert_eq!(Int::from_f64(-0.0), Int::from_f64(0.0));
    }

    #[test]
    fn integer_arithmetic_matches_i128_reference() {
        let values = [0.0, 1.0, -1.0, 2.5, -179.75, 89.125, 1024.0, -0.5];
        let scaled = |v: f64| (v * 8.0) as i128;
        for &x in &values {
            for &y in &values {
                let product = Int::from_f64(x).mul(&Int::from_f64(y));
                let difference = Int::from_f64(x).sub(&Int::from_f64(y));
                let zero = Int::from_f64(0.0);
                assert_eq!(product.compare(&zero), (scaled(x) * scaled(y)).cmp(&0));
                assert_eq!(difference.compare(&zero), (scaled(x) - scaled(y)).cmp(&0));
                assert_eq!(
                    Int::from_f64(x).compare(&Int::from_f64(y)),
                    x.partial_cmp(&y).unwrap()
                );
            }
        }
    }
}
