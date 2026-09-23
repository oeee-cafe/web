-- The Mac app registers its APNs token as a platform of its own.
--
-- It used to say "ios", because the value only ever chose the push service
-- and the Mac's is APNs too. That made the Macs indistinguishable from the
-- phones in the devices table. The value is added; nothing else changes, so
-- the release still serving during a deploy keeps running (it reads devices
-- by platform, and none of its reads ask for this one). src/push/mod.rs
-- sends to both through APNs.
ALTER TYPE platform_type ADD VALUE IF NOT EXISTS 'macos';
