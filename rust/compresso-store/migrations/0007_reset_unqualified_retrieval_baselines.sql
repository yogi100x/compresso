-- The initial 0006 writer used whole candidate-file totals and recorded
-- baselines for stale/incomplete packs. Those counterfactuals were not
-- conservative enough to retain. Preserve exact pack counts and repository
-- attribution, but reset modeled claims before the corrected writer starts.
UPDATE operations
SET estimated_raw_source_tokens = tokens_served,
    modeled_follow_up_calls_avoided = 0
WHERE estimated_raw_source_tokens IS NOT NULL;
