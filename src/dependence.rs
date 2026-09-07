//! Small, conservative affine dependence tests used before widening memory IO.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AccessKind {
    Read,
    Write,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct AffineAccess {
    /// Stable identity of the underlying pointer object.
    pub(crate) base: usize,
    /// Coefficient in `coefficient * iteration + offset`.
    pub(crate) coefficient: i64,
    pub(crate) offset: i64,
    pub(crate) kind: AccessKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Dependence {
    Independent,
    SameIteration,
    LoopCarried { distance: i64 },
    Potential,
}

/// Tests whether two one-dimensional affine accesses can name the same cell.
///
/// The GCD test proves independence when the linear Diophantine equation has
/// no integer solution. Equal coefficients additionally expose an exact
/// iteration distance. Different coefficients remain conservative because
/// loop bounds are not represented here.
pub(crate) fn classify(left: AffineAccess, right: AffineAccess) -> Dependence {
    if left.kind == AccessKind::Read && right.kind == AccessKind::Read {
        return Dependence::Independent;
    }
    if left.base != right.base {
        return Dependence::Independent;
    }

    let divisor = gcd(
        left.coefficient.unsigned_abs(),
        right.coefficient.unsigned_abs(),
    );
    let constant_delta = right.offset - left.offset;
    if divisor == 0 {
        return if constant_delta == 0 {
            Dependence::SameIteration
        } else {
            Dependence::Independent
        };
    }
    if constant_delta.unsigned_abs() % divisor != 0 {
        return Dependence::Independent;
    }

    if left.coefficient == right.coefficient && left.coefficient != 0 {
        let numerator = left.offset - right.offset;
        if numerator % left.coefficient == 0 {
            let distance = numerator / left.coefficient;
            return if distance == 0 {
                Dependence::SameIteration
            } else {
                Dependence::LoopCarried { distance }
            };
        }
    }
    Dependence::Potential
}

const fn gcd(mut left: u64, mut right: u64) -> u64 {
    while right != 0 {
        let remainder = left % right;
        left = right;
        right = remainder;
    }
    left
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn access(coefficient: i64, offset: i64, kind: AccessKind) -> AffineAccess {
        AffineAccess {
            base: 1,
            coefficient,
            offset,
            kind,
        }
    }

    #[test]
    fn read_read_pairs_never_block_vectorization() {
        let left = access(1, 0, AccessKind::Read);
        let right = access(1, -1, AccessKind::Read);
        assert_eq!(classify(left, right), Dependence::Independent);
    }

    #[test]
    fn equal_subscripts_are_same_iteration() {
        let read = access(1, 0, AccessKind::Read);
        let write = access(1, 0, AccessKind::Write);
        assert_eq!(classify(read, write), Dependence::SameIteration);
    }

    #[test]
    fn recurrence_has_exact_distance() {
        let write = access(1, 0, AccessKind::Write);
        let later_read = access(1, -1, AccessKind::Read);
        assert_eq!(
            classify(write, later_read),
            Dependence::LoopCarried { distance: 1 }
        );
    }

    #[test]
    fn gcd_can_prove_independence() {
        let even = access(2, 0, AccessKind::Write);
        let odd = access(2, 1, AccessKind::Read);
        assert_eq!(classify(even, odd), Dependence::Independent);
    }

    #[test]
    fn different_slopes_remain_conservative_when_the_gcd_test_passes() {
        let left = access(2, 0, AccessKind::Write);
        let right = access(3, 0, AccessKind::Read);
        assert_eq!(classify(left, right), Dependence::Potential);
    }
}
