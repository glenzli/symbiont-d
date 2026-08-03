BEGIN IMMEDIATE;

CREATE TABLE IF NOT EXISTS pcp_relation_retractions (
    relation_id TEXT PRIMARY KEY,
    from_revision_id TEXT NOT NULL,
    relation_type TEXT NOT NULL,
    to_revision_id TEXT NOT NULL,
    original_actor_type TEXT NOT NULL,
    original_actor_id TEXT NOT NULL,
    original_created_at TEXT NOT NULL,
    retracted_actor_type TEXT NOT NULL,
    retracted_actor_id TEXT NOT NULL,
    retracted_at TEXT NOT NULL,
    reason TEXT NOT NULL
);

CREATE TEMP TABLE mechanical_user_replies (
    from_revision_id TEXT NOT NULL,
    to_revision_id TEXT NOT NULL,
    PRIMARY KEY (from_revision_id, to_revision_id)
);

INSERT OR IGNORE INTO mechanical_user_replies (from_revision_id, to_revision_id)
SELECT relation.from_revision_id, relation.to_revision_id
FROM pcp_relations relation
JOIN pcp_revisions source ON source.revision_id = relation.from_revision_id
WHERE relation.relation_type = 'responds_to'
  AND json_extract(source.facets_json, '$.kind') = 'conversation_event'
  AND json_extract(source.facets_json, '$.role') = 'user'
  AND NOT EXISTS (
      SELECT 1
      FROM pcp_relations explicit
      WHERE explicit.from_revision_id = relation.from_revision_id
        AND explicit.to_revision_id = relation.to_revision_id
        AND explicit.relation_type = 'quotes'
  );

INSERT OR IGNORE INTO pcp_relation_retractions (
    relation_id, from_revision_id, relation_type, to_revision_id,
    original_actor_type, original_actor_id, original_created_at,
    retracted_actor_type, retracted_actor_id, retracted_at, reason
)
SELECT relation_id, from_revision_id, relation_type, to_revision_id,
       actor_type, actor_id, created_at,
       'system', 'system:symbiont-conversation-repair',
       '2026-08-04T00:00:00+08:00', 'mechanical_temporal_adjacency'
FROM pcp_relations
WHERE relation_type = 'follows';

INSERT OR IGNORE INTO pcp_relation_retractions (
    relation_id, from_revision_id, relation_type, to_revision_id,
    original_actor_type, original_actor_id, original_created_at,
    retracted_actor_type, retracted_actor_id, retracted_at, reason
)
SELECT relation.relation_id, relation.from_revision_id, relation.relation_type,
       relation.to_revision_id, relation.actor_type, relation.actor_id,
       relation.created_at, 'system', 'system:symbiont-conversation-repair',
       '2026-08-04T00:00:00+08:00', 'assumed_user_reply'
FROM pcp_relations relation
JOIN mechanical_user_replies repair
  ON repair.from_revision_id = relation.from_revision_id
 AND repair.to_revision_id = relation.to_revision_id
WHERE relation.relation_type = 'responds_to';

UPDATE pcp_revisions AS revision
SET provenance_json = (
    SELECT json_group_array(json(
        CASE
            WHEN json_type(event.value, '$.inputRevisionIds') IS NOT NULL THEN
                json_set(
                    event.value,
                    '$.inputRevisionIds',
                    json(COALESCE((
                        SELECT json_group_array(input.value)
                        FROM json_each(event.value, '$.inputRevisionIds') input
                        WHERE NOT EXISTS (
                            SELECT 1
                            FROM mechanical_user_replies repair
                            WHERE repair.from_revision_id = revision.revision_id
                              AND repair.to_revision_id = CAST(input.value AS TEXT)
                        )
                    ), '[]'))
                )
            ELSE event.value
        END
    ))
    FROM json_each(revision.provenance_json) event
)
WHERE EXISTS (
    SELECT 1 FROM mechanical_user_replies repair
    WHERE repair.from_revision_id = revision.revision_id
);

UPDATE pcp_revisions AS revision
SET provenance_json = (
    SELECT json_group_array(json(
        CASE
            WHEN json_type(event.value, '$.inputPageIds') IS NOT NULL THEN
                json_set(
                    event.value,
                    '$.inputPageIds',
                    json(COALESCE((
                        SELECT json_group_array(input.value)
                        FROM json_each(event.value, '$.inputPageIds') input
                        WHERE NOT EXISTS (
                            SELECT 1
                            FROM mechanical_user_replies repair
                            WHERE repair.from_revision_id = revision.revision_id
                              AND repair.to_revision_id = CAST(input.value AS TEXT)
                        )
                    ), '[]'))
                )
            ELSE event.value
        END
    ))
    FROM json_each(revision.provenance_json) event
)
WHERE EXISTS (
    SELECT 1 FROM mechanical_user_replies repair
    WHERE repair.from_revision_id = revision.revision_id
);

DELETE FROM pcp_provenance_inputs
WHERE EXISTS (
    SELECT 1
    FROM mechanical_user_replies repair
    WHERE repair.from_revision_id = pcp_provenance_inputs.derived_revision_id
      AND repair.to_revision_id = pcp_provenance_inputs.input_revision_id
);

DELETE FROM pcp_relations WHERE relation_type = 'follows';

DELETE FROM pcp_relations
WHERE relation_type = 'responds_to'
  AND EXISTS (
      SELECT 1
      FROM mechanical_user_replies repair
      WHERE repair.from_revision_id = pcp_relations.from_revision_id
        AND repair.to_revision_id = pcp_relations.to_revision_id
  );

INSERT INTO pcp_metadata (key, value)
VALUES (
    'symbiont_conversation_relation_repair',
    json_object(
        'version', 1,
        'appliedAt', '2026-08-04T00:00:00+08:00',
        'retractedRelations', (
            SELECT count(*) FROM pcp_relation_retractions
            WHERE retracted_actor_id = 'system:symbiont-conversation-repair'
        )
    )
)
ON CONFLICT(key) DO UPDATE SET value = excluded.value;

DROP TABLE mechanical_user_replies;

COMMIT;
