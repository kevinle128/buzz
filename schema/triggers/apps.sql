
CREATE TRIGGER apps_created_by_guard
BEFORE UPDATE OF created_by, community_id, id ON apps
FOR EACH ROW
EXECUTE FUNCTION prevent_app_created_by_update();
