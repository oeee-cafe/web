-- A value cannot be dropped from an enum, so the type is made again without
-- it. The Macs' tokens go back to being "ios", which is what they were
-- registered as before; one already there under "ios" as well is kept once.
DELETE FROM devices AS mac
WHERE mac.platform = 'macos'
  AND EXISTS (
    SELECT 1 FROM devices AS phone
    WHERE phone.platform = 'ios' AND phone.device_token = mac.device_token
  );
UPDATE devices SET platform = 'ios' WHERE platform = 'macos';

ALTER TYPE platform_type RENAME TO platform_type_with_macos;
CREATE TYPE platform_type AS ENUM ('ios', 'android');
ALTER TABLE devices
  ALTER COLUMN platform TYPE platform_type USING platform::text::platform_type;
DROP TYPE platform_type_with_macos;
