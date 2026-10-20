ALTER TABLE doctor_number_state
  ADD COLUMN fingerprint_key_ciphertext bytea,
  ADD COLUMN fingerprint_key_nonce bytea,
  ADD CONSTRAINT doctor_number_fingerprint_key_pair CHECK (
    (fingerprint_key_ciphertext IS NULL AND fingerprint_key_nonce IS NULL)
    OR (octet_length(fingerprint_key_ciphertext)=48 AND octet_length(fingerprint_key_nonce)=12)
  );
