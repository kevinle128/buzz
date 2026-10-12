CREATE FUNCTION prevent_app_created_by_update()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF NEW.created_by IS DISTINCT FROM OLD.created_by THEN
        RAISE EXCEPTION 'apps.created_by is immutable'
            USING ERRCODE = 'integrity_constraint_violation';
    END IF;
    IF NEW.community_id IS DISTINCT FROM OLD.community_id
       OR NEW.id IS DISTINCT FROM OLD.id THEN
        RAISE EXCEPTION 'apps identity is immutable'
            USING ERRCODE = 'integrity_constraint_violation';
    END IF;
    RETURN NEW;
END;
$$;
