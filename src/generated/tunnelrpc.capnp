using Go = import "go.capnp";

@0xdb8274f9144abc7e;

struct ClientInfo {
    # The tunnel client's unique identifier, used to verify a reconnection.
    clientId @0 :Data;
    # Set of features this cloudflared knows it supports
    features @1 :List(Text);
    # Information about the running binary.
    version @2 :Text;
    # Client OS and CPU info
    arch @3 :Text;
}

struct ConnectionOptions {
    # client details
    client @0 :ClientInfo;
    # origin LAN IP
    originLocalIp @1 :Data;
    # What to do if connection already exists
    replaceExisting @2 :Bool;
    # cross stream compression setting, 0 - off, 3 - high
    compressionQuality @3 :UInt8;
    # number of previous attempts to send RegisterConnection
    numPreviousAttempts @4 :UInt8;
}

struct ConnectionResponse {
    result :union {
        error @0 :ConnectionError;
        connectionDetails @1 :ConnectionDetails;
    }
}

struct ConnectionError {
    cause @0 :Text;
    # How long should this connection wait to retry in ns
    retryAfter @1 :Int64;
    shouldRetry @2 :Bool;
}

struct ConnectionDetails {
    # identifier of this connection
    uuid @0 :Data;
    # airport code of the colo where this connection landed
    locationName @1 :Text;
    # tells if the tunnel is remotely managed
    tunnelIsRemotelyManaged @2: Bool;
}

struct TunnelAuth {
    accountTag @0 :Text;
    tunnelSecret @1 :Data;
}

interface RegistrationServer {
    registerConnection @0 (auth :TunnelAuth, tunnelId :Data, connIndex :UInt8, options :ConnectionOptions) -> (result :ConnectionResponse);
}
