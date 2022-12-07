CREATE TABLE keys (
    public_key      BYTES NOT NULL,
    seed            BYTES NOT NULL,
    PRIMARY KEY (public_key)
);

CREATE TABLE channel_managers (
    timestamp       TIMESTAMP NOT NULL DEFAULT current_timestamp(),
    manager         BYTES NOT NULL
);

CREATE TABLE channel_monitors (
    out_point       BYTES NOT NULL,
    update_id       INT NOT NULL,
    timestamp       TIMESTAMP NOT NULL DEFAULT current_timestamp(),
    monitor         BYTES NOT NULL,
    PRIMARY KEY ( out_point )
);

CREATE TABLE channel_monitor_updates (
    out_point       BYTES NOT NULL,
    update          BYTES NOT NULL,
    update_id       INT NOT NULL,
    timestamp       TIMESTAMP NOT NULL DEFAULT current_timestamp(),
    PRIMARY KEY ( out_point, update_id )
);

CREATE TABLE network_graph (
    timestamp       TIMESTAMP NOT NULL DEFAULT current_timestamp(),
    graph           BYTES NOT NULL
);

CREATE TABLE scorer (
    timestamp       TIMESTAMP NOT NULL DEFAULT current_timestamp(),
    scorer          BYTES NOT NULL
);

CREATE TABLE peers (
    public_key      BYTES NOT NULL,
    address         BYTES NOT NULL,
    PRIMARY KEY ( public_key, address )
);

GRANT SELECT ON TABLE peers TO grafana;

CREATE TYPE htlc_status AS ENUM ('succeeded', 'failed');

CREATE TABLE payments (
    hash            BYTES NOT NULL,
    preimage        BYTES,
    secret          BYTES,
    status          htlc_status NOT NULL,
    amount_msat     INT NOT NULL,
    is_outbound     BOOL NOT NULL,
    timestamp       TIMESTAMP NOT NULL DEFAULT current_timestamp(),
    PRIMARY KEY ( preimage )
);
