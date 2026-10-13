CREATE TABLE IF NOT EXISTS legacy_domain_records (
    domain TEXT NOT NULL,
    source_table TEXT NOT NULL,
    record_key TEXT NOT NULL,
    record_json TEXT NOT NULL,
    PRIMARY KEY(domain, source_table, record_key)
);

CREATE INDEX IF NOT EXISTS idx_legacy_domain_records_table
    ON legacy_domain_records(domain, source_table);
