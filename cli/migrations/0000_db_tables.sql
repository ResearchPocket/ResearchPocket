CREATE TABLE if not exists providers (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL,
    secret TEXT
);
CREATE TABLE if not exists items (
    id INTEGER PRIMARY KEY,
    uri TEXT UNIQUE,
    title TEXT,
    excerpt TEXT,
    -- summary
    time_added INTEGER,
    -- timestamp as unix time
    favorite BOOLEAN,
    lang TEXT,
    provider_id INTEGER,
    FOREIGN KEY(provider_id) REFERENCES providers(id)
);
CREATE TABLE if not exists tags (tag_name TEXT PRIMARY KEY);
CREATE TABLE if not exists item_tags (
    item_id INTEGER,
    tag_name TEXT,
    FOREIGN KEY(item_id) REFERENCES items(id),
    FOREIGN KEY(tag_name) REFERENCES tags(tag_name)
);