# One deployment serves one Practice

The original spec talks about a "platform-managed" integration and "per-doctor credentials", which reads as a multi-tenant product, but v1 serves one independent doctor. Each deployment serves exactly one Practice: no tenant column, one WAHA session, one stored Google connection; a second doctor gets a second deployment. This avoids tenant isolation work (row scoping, per-tenant credential isolation, cross-tenant leak testing) for a product with one customer. WAHA licensing was not a factor: multi-session has been free in WAHA Core since 2026.6.1.

## Consequences

Moving to multi-tenancy later means adding Practice scoping to every table, query and background job.
