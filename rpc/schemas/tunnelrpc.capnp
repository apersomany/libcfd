# Cloudflare Tunnel registration RPC schema (libcfd).
#
# Field ordinals, explicit type ids and the interface id are wire-compatible
# with cloudflared's tunnelrpc/proto/tunnelrpc.capnp.

@0xdb8274f9144abc7e;

struct ClientInfo @0x83ced0145b2f114b {
    clientId @0 :Data;
    features @1 :List(Text);
    version @2 :Text;
    arch @3 :Text;
}

struct ConnectionOptions @0xb4bf9861fe035d04 {
    client @0 :ClientInfo;
    originLocalIp @1 :Data;
    replaceExisting @2 :Bool;
    compressionQuality @3 :UInt8;
    numPreviousAttempts @4 :UInt8;
}

struct ConnectionResponse @0xdbaa9d03d52b62dc {
    result :union {
        error @0 :ConnectionError;
        connectionDetails @1 :ConnectionDetails;
    }
}

struct ConnectionError @0xf5f383d2785edb86 {
    cause @0 :Text;
    retryAfter @1 :Int64;
    shouldRetry @2 :Bool;
}

struct ConnectionDetails @0xb5f39f082b9ac18a {
    uuid @0 :Data;
    locationName @1 :Text;
    tunnelIsRemotelyManaged @2 :Bool;
}

struct TunnelAuth @0x9496331ab9cd463f {
    accountTag @0 :Text;
    tunnelSecret @1 :Data;
}

interface RegistrationServer @0xf71695ec7fe85497 {
    registerConnection @0 (auth :TunnelAuth, tunnelId :Data, connIndex :UInt8, options :ConnectionOptions) -> (result :ConnectionResponse);
    unregisterConnection @1 () -> ();
    updateLocalConfiguration @2 (config :Data) -> ();
}

struct RegisterUdpSessionResponse @0xab6d5210c1f26687 {
    err @0 :Text;
    spans @1 :Data;
}

interface SessionManager @0x839445a59fb01686 {
    registerUdpSession @0 (sessionId :Data, dstIp :Data, dstPort :UInt16, closeAfterIdleHint :Int64, traceContext :Text = "") -> (result :RegisterUdpSessionResponse);
    unregisterUdpSession @1 (sessionId :Data, message :Text) -> ();
}

struct UpdateConfigurationResponse @0xdb58ff694ba05cf9 {
    latestAppliedVersion @0 :Int32;
    err @1 :Text;
}

interface ConfigurationManager @0xb48edfbdaa25db04 {
    updateConfiguration @0 (version :Int32, config :Data) -> (result :UpdateConfigurationResponse);
}

interface CloudflaredServer @0xf548cef9dea2a4a1 extends(SessionManager, ConfigurationManager) {}
