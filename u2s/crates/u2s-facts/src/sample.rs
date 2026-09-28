//! [`select_sample`]: the inputs a draft fact revision is extracted on
//! before it is saved.

use u2s_core::InputId;

/// How many inputs a draft fact is extracted on, at least. Small enough that
/// every workbench iteration that changes a question costs a handful of
/// extractions rather than a corpus backfill; large enough that the
/// stability gate sees more than one form.
pub const FACT_SAMPLE_INPUTS: usize = 12;

/// The draft sample, in priority order, without duplicates.
///
/// Every input behind an accepted run is included, and so is the input of
/// the run a feedback rule was written to catch, even past `limit`: those
/// are the verdicts the save gate and the author care about most, and
/// leaving one out would make its draft verdict `indeterminate` exactly
/// where it matters. `recent` then fills up to `limit`.
pub fn select_sample(
    accepted: &[InputId],
    target: Option<InputId>,
    recent: &[InputId],
    limit: usize,
) -> Vec<InputId> {
    let mut sample: Vec<InputId> = Vec::with_capacity(limit.max(accepted.len() + 1));
    let push = |id: InputId, sample: &mut Vec<InputId>| {
        if !sample.contains(&id) {
            sample.push(id);
        }
    };
    for id in accepted {
        push(*id, &mut sample);
    }
    if let Some(id) = target {
        push(id, &mut sample);
    }
    for id in recent {
        if sample.len() >= limit {
            break;
        }
        push(*id, &mut sample);
    }
    sample
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(n: usize) -> Vec<InputId> {
        (0..n).map(|_| InputId::generate()).collect()
    }

    #[test]
    fn accepted_and_target_come_first_then_recent_fills_up() {
        let accepted = ids(2);
        let target = InputId::generate();
        let recent = ids(5);
        let sample = select_sample(&accepted, Some(target), &recent, 4);
        assert_eq!(sample, vec![accepted[0], accepted[1], target, recent[0]]);
    }

    #[test]
    fn accepted_inputs_are_never_cut_by_the_limit() {
        let accepted = ids(5);
        let sample = select_sample(&accepted, None, &ids(3), 2);
        assert_eq!(sample, accepted);
    }

    #[test]
    fn duplicates_across_the_lists_appear_once() {
        let shared = InputId::generate();
        let sample = select_sample(&[shared], Some(shared), &[shared], 10);
        assert_eq!(sample, vec![shared]);
    }
}
