CREATE TABLE if not exists providers (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL,
    secret TEXT
);
CREATE TABLE if not exists items (
    id INTEGER PRIMARY KEY,
    uri TEXT UNIQUE,
    title TEXT,
    -- summary
    excerpt TEXT,
    -- timestamp as unix time
    time_added INTEGER,
    favorite BOOLEAN,
    lang TEXT,
    provider_id INTEGER,
    FOREIGN KEY(provider_id) REFERENCES providers(id)
);
CREATE TABLE if not exists tags (tag_name TEXT PRIMARY KEY);
CREATE TABLE if not exists item_tags (
    item_id INTEGER,
    tag_name TEXT,
    PRIMARY KEY(item_id, tag_name),
    FOREIGN KEY(item_id) REFERENCES items(id),
    FOREIGN KEY(tag_name) REFERENCES tags(tag_name)
);