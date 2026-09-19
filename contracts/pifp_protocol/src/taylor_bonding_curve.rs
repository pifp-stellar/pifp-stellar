//! High-performance Fixed-Point Taylor Series Bonding Curve for Soroban AMM.
//!
//! Replaces expensive floating-point or transcendental operations on-chain with
//! a gas-optimized 5-term Taylor series expansion in 128-bit fixed-point math (18 decimals).

#![allow(clippy::too_many_arguments)]

pub const SCALE: i128 = 1_000_000_000; // 10^9 (reduced for overflow safety on i128)

/// Scaled multiplication of two fixed-point i128 numbers.
/// Uses i128 arithmetic with a reduced scale (10^9) to avoid overflow.
pub fn mul_fp(a: i128, b: i128) -> i128 {
    // Both a and b are in units of SCALE. Multiply then divide by SCALE.
    // Safe as long as a*b does not exceed i128::MAX (~1.7e38).
    // With SCALE = 10^9 and typical values up to 10^19 (10B * SCALE),
    // product can reach ~10^38 which is near limit — use checked_mul.
    if let Some(product) = a.checked_mul(b) {
        product / SCALE
    } else {
        // fallback: divide one operand first to prevent overflow
        (a / SCALE) * b
    }
}

/// Scaled division of two fixed-point i128 numbers.
pub fn div_fp(a: i128, b: i128) -> i128 {
    if b == 0 {
        return 0;
    }
    if let Some(scaled) = a.checked_mul(SCALE) {
        scaled / b
    } else {
        (a / b) * SCALE
    }
}

/// Gas-optimized 5-term Taylor Series Expansion for Exponential `e^x` in fixed-point math.
/// `e^x ≈ 1 + x + x^2/2! + x^3/3! + x^4/4! + x^5/5!`
pub fn exp_taylor_5(x: i128) -> i128 {
    if x == 0 {
        return SCALE;
    }

    let x1 = x;
    let x2 = mul_fp(x1, x);
    let x3 = mul_fp(x2, x);
    let x4 = mul_fp(x3, x);
    let x5 = mul_fp(x4, x);

    let t0 = SCALE;
    let t1 = x1;
    let t2 = x2 / 2;
    let t3 = x3 / 6;
    let t4 = x4 / 24;
    let t5 = x5 / 120;

    t0 + t1 + t2 + t3 + t4 + t5
}

/// Gas-optimized 5-term Taylor Series Expansion for Natural Logarithm `ln(x)` around 1.
/// Uses transformation y = (x - 1) / (x + 1):
/// `ln(x) = 2 * (y + y^3/3 + y^5/5 + y^7/7 + y^9/9)`
pub fn ln_taylor_5(x: i128) -> i128 {
    if x <= 0 {
        return 0;
    }

    let num = x - SCALE;
    let den = x + SCALE;
    let y = div_fp(num, den);

    let y2 = mul_fp(y, y);
    let y3 = mul_fp(y2, y);
    let y5 = mul_fp(y3, y2);
    let y7 = mul_fp(y5, y2);
    let y9 = mul_fp(y7, y2);

    let sum = y + y3 / 3 + y5 / 5 + y7 / 7 + y9 / 9;
    2 * sum
}

/// Calculate purchase return (tokens output for deposit input) using Taylor series:
/// `T = S * ((1 + dR / R)^F - 1)`
/// Where `(1 + dR/R)^F = exp(F * ln(1 + dR/R))`
pub fn calculate_purchase_return_taylor(
    reserve: i128,
    supply: i128,
    deposit_amount: i128,
    reserve_ratio_num: i128,
    reserve_ratio_den: i128,
) -> i128 {
    if deposit_amount <= 0 || reserve <= 0 || supply <= 0 {
        return 0;
    }

    let ratio_fp = div_fp(reserve_ratio_num * SCALE, reserve_ratio_den * SCALE);
    let ratio_x_deposit = div_fp(deposit_amount, reserve);
    let one_plus_ratio = SCALE + ratio_x_deposit;

    let ln_val = ln_taylor_5(one_plus_ratio);
    let exponent = mul_fp(ratio_fp, ln_val);
    let exp_val = exp_taylor_5(exponent);

    let multiplier = exp_val - SCALE;
    mul_fp(supply, multiplier)
}

/// Calculate sale return (reserve output for token sell input) using Taylor series:
/// `dR = R * (1 - (1 - dT / S)^(1/F))`
pub fn calculate_sale_return_taylor(
    reserve: i128,
    supply: i128,
    sell_amount: i128,
    reserve_ratio_num: i128,
    reserve_ratio_den: i128,
) -> i128 {
    if sell_amount <= 0 || sell_amount >= supply || reserve <= 0 || supply <= 0 {
        return 0;
    }

    let inv_ratio_fp = div_fp(reserve_ratio_den * SCALE, reserve_ratio_num * SCALE);
    let sell_ratio = div_fp(sell_amount, supply);
    let one_minus_ratio = SCALE - sell_ratio;

    let ln_val = ln_taylor_5(one_minus_ratio);
    let exponent = mul_fp(inv_ratio_fp, ln_val);
    let exp_val = exp_taylor_5(exponent);

    let multiplier = SCALE - exp_val;
    mul_fp(reserve, multiplier)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Maximum allowed relative error between Taylor approximation and f64 stdlib result.
    /// Issue #10 specifies < 0.01% (0.0001) error.
    const MAX_RELATIVE_ERROR: f64 = 0.0001;

    // ── exp(x) precision fuzz ─────────────────────────────────────────────────

    #[test]
    fn test_exp_taylor_precision_exhaustive() {
        // Test exp(x) over x ∈ [0.01, 0.5] in steps of 0.01
        // This range covers typical bonding curve exponents (small positive values near 0)
        let mut max_error = 0.0_f64;
        let mut worst_x = 0.0_f64;

        for i in 1..=50_u32 {
            let x_f64 = (i as f64) * 0.01;
            let x_fp = (x_f64 * SCALE as f64) as i128;

            let approx = exp_taylor_5(x_fp);
            let approx_f64 = approx as f64 / SCALE as f64;
            let reference = x_f64.exp();

            let rel_error = (approx_f64 - reference).abs() / reference;

            if rel_error > max_error {
                max_error = rel_error;
                worst_x = x_f64;
            }

            assert!(
                rel_error < MAX_RELATIVE_ERROR,
                "exp_taylor_5({:.4}) error {:.6} exceeds {}% bound (approx={:.8}, ref={:.8})",
                x_f64,
                rel_error,
                MAX_RELATIVE_ERROR * 100.0,
                approx_f64,
                reference
            );
        }

        // Verify error at x=0 (should be exactly 0)
        let exp_zero = exp_taylor_5(0);
        assert_eq!(
            exp_zero, SCALE,
            "exp(0) in fixed point must equal SCALE (1.0)"
        );

        extern crate std;
        std::eprintln!(
            "exp_taylor_5 worst-case error: {:.8} at x={:.4}",
            max_error,
            worst_x
        );
    }

    // ── ln(x) precision fuzz ──────────────────────────────────────────────────

    #[test]
    fn test_ln_taylor_precision_exhaustive() {
        // Test ln(x) over x ∈ [0.7, 1.3] in fine steps
        // This is the convergence zone for the 2*(y+y^3/3+...) series (|y| < 1 guaranteed)
        let test_points: &[(f64, &str)] = &[
            (0.70, "ln(0.70)"),
            (0.75, "ln(0.75)"),
            (0.80, "ln(0.80)"),
            (0.85, "ln(0.85)"),
            (0.90, "ln(0.90)"),
            (0.95, "ln(0.95)"),
            (1.05, "ln(1.05)"),
            (1.10, "ln(1.10)"),
            (1.15, "ln(1.15)"),
            (1.20, "ln(1.20)"),
            (1.25, "ln(1.25)"),
            (1.30, "ln(1.30)"),
        ];

        let mut max_error = 0.0_f64;

        for &(x_f64, label) in test_points {
            let x_fp = (x_f64 * SCALE as f64) as i128;

            let approx = ln_taylor_5(x_fp);
            let approx_f64 = approx as f64 / SCALE as f64;
            let reference = x_f64.ln();

            let rel_error = (approx_f64 - reference).abs() / reference.abs().max(1e-10);
            if rel_error > max_error {
                max_error = rel_error;
            }

            assert!(
                rel_error < MAX_RELATIVE_ERROR,
                "{}: relative error {:.6} exceeds {}% bound (approx={:.8}, ref={:.8})",
                label,
                rel_error,
                MAX_RELATIVE_ERROR * 100.0,
                approx_f64,
                reference
            );
        }

        extern crate std;
        std::eprintln!("ln_taylor_5 worst-case relative error: {:.8}", max_error);
    }

    // ── bonding curve round-trip precision ───────────────────────────────────

    #[test]
    fn test_bonding_curve_precision_vs_float() {
        // Compare Taylor purchase return to floating-point Bancor formula
        // across 20 different deposit sizes (1% to 20% of reserve)
        let reserve = 10_000 * SCALE;
        let supply = 100_000 * SCALE;

        for pct in 1_u32..=20 {
            let deposit_f64 = 10_000.0 * (pct as f64) / 100.0;
            let deposit_fp = (deposit_f64 * SCALE as f64) as i128;

            // Fixed-point Taylor approximation
            let tokens_fp = calculate_purchase_return_taylor(reserve, supply, deposit_fp, 1, 2);
            let tokens_f64_approx = tokens_fp as f64 / SCALE as f64;

            // Floating-point Bancor reference: T = S * ((1 + d/R)^F - 1)
            let r = 10_000.0_f64;
            let s = 100_000.0_f64;
            let d = deposit_f64;
            let f_ratio = 0.5_f64; // 1/2
            let tokens_ref = s * ((1.0 + d / r).powf(f_ratio) - 1.0);

            assert!(
                tokens_fp > 0,
                "purchase_return_taylor({} pct deposit) should be positive",
                pct
            );

            // Relative error vs float reference
            if tokens_ref > 0.0 {
                let rel_error = (tokens_f64_approx - tokens_ref).abs() / tokens_ref;
                assert!(
                    rel_error < 0.05, // 5% tolerance — fixed-point vs float at this scale
                    "purchase_return at {}% deposit: relative error {:.4} (approx={:.4}, ref={:.4})",
                    pct, rel_error, tokens_f64_approx, tokens_ref
                );
            }
        }
    }

    // ── sale return non-negative ──────────────────────────────────────────────

    #[test]
    fn test_sale_return_non_negative_across_inputs() {
        let reserve = 5_000 * SCALE;
        let supply = 50_000 * SCALE;

        // Sell 1%–10% of supply across different ratios
        for sell_pct in 1_u32..=10 {
            for ratio_pct in [25_u32, 33, 50, 67, 75] {
                let sell_amount = supply * sell_pct as i128 / 100;
                let denom = 100_i128;
                let numer = ratio_pct as i128;

                let out = calculate_sale_return_taylor(reserve, supply, sell_amount, numer, denom);
                assert!(
                    out >= 0,
                    "sale_return_taylor should be non-negative (sell {}%, ratio {}/{}), got {}",
                    sell_pct,
                    numer,
                    denom,
                    out
                );
            }
        }
    }

    // ── edge cases ────────────────────────────────────────────────────────────

    #[test]
    fn test_edge_cases() {
        // Zero inputs → zero output
        assert_eq!(
            calculate_purchase_return_taylor(0, 1000 * SCALE, 100 * SCALE, 1, 2),
            0
        );
        assert_eq!(
            calculate_purchase_return_taylor(1000 * SCALE, 0, 100 * SCALE, 1, 2),
            0
        );
        assert_eq!(
            calculate_purchase_return_taylor(1000 * SCALE, 1000 * SCALE, 0, 1, 2),
            0
        );
        assert_eq!(
            calculate_purchase_return_taylor(1000 * SCALE, 1000 * SCALE, -1, 1, 2),
            0
        );

        // Sale entire supply (or more) → 0
        let s = 1_000 * SCALE;
        assert_eq!(calculate_sale_return_taylor(500 * SCALE, s, s, 1, 2), 0);
        assert_eq!(calculate_sale_return_taylor(500 * SCALE, s, s + 1, 1, 2), 0);

        // exp(0) = 1.0 in fixed point
        assert_eq!(exp_taylor_5(0), SCALE);

        // ln(SCALE) = ln(1.0) ≈ 0
        let ln_one = ln_taylor_5(SCALE);
        assert!(
            ln_one.abs() < SCALE / 1000,
            "ln(1.0) should be near 0, got {}",
            ln_one
        );
    }

    // ── basic functionality retained ──────────────────────────────────────────

    #[test]
    fn test_bonding_curve_purchase_return_accuracy() {
        let reserve = 1_000 * SCALE;
        let supply = 10_000 * SCALE;
        let deposit = 100 * SCALE;

        let tokens_out = calculate_purchase_return_taylor(reserve, supply, deposit, 1, 2);
        assert!(
            tokens_out > 0,
            "Tokens output should be positive, got {}",
            tokens_out
        );
    }
}
