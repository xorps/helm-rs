//! Just enough of `github.com/shopspring/decimal` (and `math/big`) for
//! Sprig's `addf`/`subf`/`mulf`/`divf`: exact decimal arithmetic on values
//! created with `NewFromFloat`, division rounded to 16 places (half away from
//! zero), and conversion back with `Float64()` (nearest float).

#![allow(
    clippy::needless_range_loop,
    clippy::collapsible_match,
    clippy::too_many_arguments,
    clippy::type_complexity
)]

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::cmp::Ordering;
use core::fmt::Write as _;

use crate::func::FuncError;

/// Unsigned big integer, little-endian base 2^32 limbs, no trailing zeros.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BigUint(Vec<u32>);

impl BigUint {
    pub fn zero() -> BigUint {
        BigUint(Vec::new())
    }
    pub fn from_u64(v: u64) -> BigUint {
        let mut b = BigUint(alloc::vec![v as u32, (v >> 32) as u32]);
        b.trim();
        b
    }
    fn trim(&mut self) {
        while self.0.last() == Some(&0) {
            self.0.pop();
        }
    }
    pub fn is_zero(&self) -> bool {
        self.0.is_empty()
    }
    pub fn cmp(&self, o: &BigUint) -> Ordering {
        if self.0.len() != o.0.len() {
            return self.0.len().cmp(&o.0.len());
        }
        for i in (0..self.0.len()).rev() {
            match self.0[i].cmp(&o.0[i]) {
                Ordering::Equal => {}
                x => return x,
            }
        }
        Ordering::Equal
    }
    pub fn add(&self, o: &BigUint) -> BigUint {
        let n = self.0.len().max(o.0.len());
        let mut out = Vec::with_capacity(n + 1);
        let mut carry = 0u64;
        for i in 0..n {
            let s = *self.0.get(i).unwrap_or(&0) as u64 + *o.0.get(i).unwrap_or(&0) as u64 + carry;
            out.push(s as u32);
            carry = s >> 32;
        }
        if carry > 0 {
            out.push(carry as u32);
        }
        let mut b = BigUint(out);
        b.trim();
        b
    }
    /// self - o, requires self >= o.
    pub fn sub(&self, o: &BigUint) -> BigUint {
        let mut out = Vec::with_capacity(self.0.len());
        let mut borrow = 0i64;
        for i in 0..self.0.len() {
            let mut d = self.0[i] as i64 - *o.0.get(i).unwrap_or(&0) as i64 - borrow;
            if d < 0 {
                d += 1 << 32;
                borrow = 1;
            } else {
                borrow = 0;
            }
            out.push(d as u32);
        }
        let mut b = BigUint(out);
        b.trim();
        b
    }
    pub fn mul_small(&self, m: u32) -> BigUint {
        let mut out = Vec::with_capacity(self.0.len() + 1);
        let mut carry = 0u64;
        for &x in &self.0 {
            let p = x as u64 * m as u64 + carry;
            out.push(p as u32);
            carry = p >> 32;
        }
        if carry > 0 {
            out.push(carry as u32);
        }
        let mut b = BigUint(out);
        b.trim();
        b
    }
    pub fn mul(&self, o: &BigUint) -> BigUint {
        if self.is_zero() || o.is_zero() {
            return BigUint::zero();
        }
        let mut out = alloc::vec![0u32; self.0.len() + o.0.len()];
        for (i, &a) in self.0.iter().enumerate() {
            let mut carry = 0u64;
            for (j, &b) in o.0.iter().enumerate() {
                let t = a as u64 * b as u64 + out[i + j] as u64 + carry;
                out[i + j] = t as u32;
                carry = t >> 32;
            }
            let mut k = i + o.0.len();
            while carry > 0 {
                let t = out[k] as u64 + carry;
                out[k] = t as u32;
                carry = t >> 32;
                k += 1;
            }
        }
        let mut b = BigUint(out);
        b.trim();
        b
    }
    /// Division by a small divisor: (quotient, remainder).
    pub fn divmod_small(&self, d: u32) -> (BigUint, u32) {
        let mut out = alloc::vec![0u32; self.0.len()];
        let mut rem = 0u64;
        for i in (0..self.0.len()).rev() {
            let cur = (rem << 32) | self.0[i] as u64;
            out[i] = (cur / d as u64) as u32;
            rem = cur % d as u64;
        }
        let mut b = BigUint(out);
        b.trim();
        (b, rem as u32)
    }
    fn bits(&self) -> usize {
        match self.0.last() {
            None => 0,
            Some(&top) => (self.0.len() - 1) * 32 + (32 - top.leading_zeros() as usize),
        }
    }
    fn shl(&self, n: usize) -> BigUint {
        let (words, bits) = (n / 32, n % 32);
        let mut out = alloc::vec![0u32; words];
        if bits == 0 {
            out.extend_from_slice(&self.0);
        } else {
            let mut carry = 0u32;
            for &x in &self.0 {
                out.push((x << bits) | carry);
                carry = x >> (32 - bits);
            }
            if carry > 0 {
                out.push(carry);
            }
        }
        let mut b = BigUint(out);
        b.trim();
        b
    }
    fn bit(&self, i: usize) -> bool {
        self.0.get(i / 32).is_some_and(|w| (w >> (i % 32)) & 1 == 1)
    }
    /// Truncating division: (quotient, remainder). `d` must be non-zero.
    pub fn divmod(&self, d: &BigUint) -> (BigUint, BigUint) {
        if d.0.len() == 1 {
            let (q, r) = self.divmod_small(d.0[0]);
            return (q, BigUint::from_u64(r as u64));
        }
        if self.cmp(d) == Ordering::Less {
            return (BigUint::zero(), self.clone());
        }
        // Binary long division over the bit-length difference (operands
        // here are at most a few thousand bits).
        let shift = self.bits() - d.bits();
        let mut rem = self.clone();
        let mut q = alloc::vec![0u32; shift / 32 + 1];
        let mut dd = d.shl(shift);
        for i in (0..=shift).rev() {
            if rem.cmp(&dd) != Ordering::Less {
                rem = rem.sub(&dd);
                q[i / 32] |= 1 << (i % 32);
            }
            dd = dd.shr1();
        }
        let mut qb = BigUint(q);
        qb.trim();
        let _ = qb.bit(0);
        (qb, rem)
    }
    fn shr1(&self) -> BigUint {
        let mut out = alloc::vec![0u32; self.0.len()];
        for i in 0..self.0.len() {
            let hi = if i + 1 < self.0.len() {
                self.0[i + 1] << 31
            } else {
                0
            };
            out[i] = (self.0[i] >> 1) | hi;
        }
        let mut b = BigUint(out);
        b.trim();
        b
    }
    pub fn pow10(n: u64) -> BigUint {
        let mut r = BigUint::from_u64(1);
        let mut n = n;
        while n >= 9 {
            r = r.mul_small(1_000_000_000);
            n -= 9;
        }
        let mut p = 1u32;
        for _ in 0..n {
            p *= 10;
        }
        r.mul_small(p)
    }
    pub fn to_decimal(&self) -> String {
        if self.is_zero() {
            return String::from("0");
        }
        let mut chunks = Vec::new();
        let mut cur = self.clone();
        while !cur.is_zero() {
            let (q, r) = cur.divmod_small(1_000_000_000);
            chunks.push(r);
            cur = q;
        }
        let mut s = String::new();
        let _ = write!(s, "{}", chunks[chunks.len() - 1]);
        for c in chunks.iter().rev().skip(1) {
            let _ = write!(s, "{c:09}");
        }
        s
    }
}

/// A decimal `neg * mag * 10^exp`.
#[derive(Clone, Debug)]
pub(crate) struct Decimal {
    neg: bool,
    mag: BigUint,
    exp: i64,
}

/// Limit on decimal rescaling (digits), to keep pathological inputs from
/// exhausting memory.
const MAX_RESCALE: i64 = 100_000;

impl Decimal {
    /// `decimal.NewFromFloat`.
    pub fn from_float(f: f64) -> Result<Decimal, FuncError> {
        if f.is_nan() || f.is_infinite() {
            return Err(FuncError::new(format!(
                "Cannot create a Decimal from {}",
                crate::gofmt::format_value(&crate::Value::Float(f))
            )));
        }
        if f == 0.0 {
            return Ok(Decimal {
                neg: false,
                mag: BigUint::zero(),
                exp: 0,
            });
        }
        let mut s = String::new();
        let _ = write!(s, "{:e}", f.abs());
        let (mant, e) = s.split_once('e').unwrap_or((&s, "0"));
        let e: i64 = e.parse().unwrap_or(0);
        let digits: String = mant.chars().filter(|c| c.is_ascii_digit()).collect();
        let nd = digits.len() as i64;
        let mut mag = BigUint::zero();
        for c in digits.bytes() {
            mag = mag.mul_small(10).add(&BigUint::from_u64((c - b'0') as u64));
        }
        // value = 0.digits * 10^(e+1) = digits * 10^(e + 1 - nd)
        Ok(Decimal {
            neg: f < 0.0,
            mag,
            exp: e + 1 - nd,
        })
    }

    fn rescale(&self, exp: i64) -> Result<BigUint, FuncError> {
        let diff = self.exp - exp;
        if diff > MAX_RESCALE {
            return Err(FuncError::new("decimal: exponent difference too large"));
        }
        Ok(self.mag.mul(&BigUint::pow10(diff as u64)))
    }

    fn signed_add(an: bool, a: BigUint, bn: bool, b: BigUint) -> (bool, BigUint) {
        if an == bn {
            return (an, a.add(&b));
        }
        match a.cmp(&b) {
            Ordering::Less => (bn, b.sub(&a)),
            _ => (an, a.sub(&b)),
        }
    }

    pub fn add(&self, o: &Decimal, negate_o: bool) -> Result<Decimal, FuncError> {
        let exp = self.exp.min(o.exp);
        let a = self.rescale(exp)?;
        let b = o.rescale(exp)?;
        let on = o.neg ^ negate_o;
        let (neg, mag) = Decimal::signed_add(self.neg, a, on, b);
        Ok(Decimal {
            neg: neg && !mag.is_zero(),
            mag,
            exp,
        })
    }

    pub fn mul(&self, o: &Decimal) -> Result<Decimal, FuncError> {
        let exp = self.exp + o.exp;
        if exp > i32::MAX as i64 || exp < i32::MIN as i64 {
            return Err(FuncError::new(format!(
                "exponent {exp} overflows an int32!"
            )));
        }
        let mag = self.mag.mul(&o.mag);
        Ok(Decimal {
            neg: (self.neg ^ o.neg) && !mag.is_zero(),
            mag,
            exp,
        })
    }

    /// `Div` = `DivRound(d2, 16)`.
    pub fn div(&self, o: &Decimal) -> Result<Decimal, FuncError> {
        if o.mag.is_zero() {
            return Err(FuncError::new("decimal division by 0"));
        }
        const PREC: i64 = 16;
        let e = self.exp - o.exp + PREC;
        if e > i32::MAX as i64 || e < i32::MIN as i64 {
            return Err(FuncError::new("overflow in decimal QuoRem"));
        }
        if e.abs() > MAX_RESCALE {
            return Err(FuncError::new("decimal: exponent difference too large"));
        }
        let (aa, bb) = if e < 0 {
            (self.mag.clone(), o.mag.mul(&BigUint::pow10((-e) as u64)))
        } else {
            (self.mag.mul(&BigUint::pow10(e as u64)), o.mag.clone())
        };
        let (q, r) = aa.divmod(&bb);
        let neg = self.neg ^ o.neg;
        let r2 = r.add(&r);
        let q = if r2.cmp(&bb) == Ordering::Less {
            q
        } else {
            q.add(&BigUint::from_u64(1))
        };
        Ok(Decimal {
            neg: neg && !q.is_zero(),
            mag: q,
            exp: -PREC,
        })
    }

    /// `Float64()`: the nearest float64.
    pub fn to_f64(&self) -> f64 {
        if self.mag.is_zero() {
            return 0.0;
        }
        let s = format!(
            "{}{}e{}",
            if self.neg { "-" } else { "" },
            self.mag.to_decimal(),
            self.exp
        );
        s.parse::<f64>().unwrap_or(0.0)
    }
}
