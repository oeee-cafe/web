-- PostgreSQL cannot remove a value from an enum, so 'pickle' stays; see
-- 20251108000001_add_community_post_notification_type.down.sql.
DELETE FROM notifications WHERE notification_type = 'pickle';
