use proptest::prelude::*;
use rustqueue::*;

fn arb_job_state() -> impl Strategy<Value = JobState> {
    prop_oneof![
        Just(JobState::Created),
        Just(JobState::Waiting),
        Just(JobState::Delayed),
        Just(JobState::Active),
        Just(JobState::Completed),
        Just(JobState::Failed),
        Just(JobState::Dlq),
        Just(JobState::Cancelled),
        Just(JobState::Blocked),
    ]
}

proptest! {
    #[test]
    fn job_serialization_roundtrip(
        queue in "[a-z]{1,20}",
        name in "[a-z-]{1,30}",
        priority in -100i32..100,
    ) {
        let mut job = Job::new(queue, name, serde_json::json!({}));
        job.priority = priority;
        let json = serde_json::to_string(&job).unwrap();
        let roundtripped: Job = serde_json::from_str(&json).unwrap();
        prop_assert_eq!(job.id, roundtripped.id);
        prop_assert_eq!(job.priority, roundtripped.priority);
    }

    #[test]
    fn all_job_states_serialize(state in arb_job_state()) {
        let json = serde_json::to_string(&state).unwrap();
        let roundtripped: JobState = serde_json::from_str(&json).unwrap();
        prop_assert_eq!(state, roundtripped);
    }

    #[test]
    fn backoff_delay_calculation(
        attempt in 1u32..20,
        base_delay in 100u64..10000,
    ) {
        let delay = base_delay.checked_mul(2u64.pow(attempt.min(16)));
        prop_assert!(delay.is_some() || attempt > 16);
    }
}
