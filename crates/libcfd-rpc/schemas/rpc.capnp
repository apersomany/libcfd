# Cap'n Proto RPC wire protocol (subset needed by libcfd).
#
# This schema reproduces the wire layout of the standard Cap'n Proto RPC
# protocol as defined by capnproto's rpc.capnp (MIT licensed).  Field
# ordinals, union discriminants and the file id must stay byte-compatible
# with capnproto2 v2.18.0, which cloudflared's edge speaks.

@0xb312981b2552a250;

using QuestionId = UInt32;
using AnswerId = QuestionId;
using ExportId = UInt32;
using ImportId = ExportId;
using InterfaceId = UInt64;
using MethodId = UInt16;

struct Message {
  union {
    unimplemented @0 :Message;
    abort @1 :Exception;
    call @2 :Call;
    return @3 :Return;
    finish @4 :Finish;
    resolve @5 :Resolve;
    release @6 :Release;
    obsoleteSave @7 :AnyPointer;
    bootstrap @8 :Bootstrap;
    obsoleteDelete @9 :AnyPointer;
    provide @10 :Provide;
    accept @11 :Accept;
    join @12 :Join;
    disembargo @13 :Disembargo;
  }
}

struct Bootstrap {
  questionId @0 :QuestionId;
  deprecatedObjectId @1 :AnyPointer;
}

struct Call {
  questionId @0 :QuestionId;
  target @1 :MessageTarget;
  interfaceId @2 :InterfaceId;
  methodId @3 :MethodId;
  allowThirdPartyTailCall @8 :Bool = false;
  params @4 :Payload;
  sendResultsTo :union {
    caller @5 :Void;
    yourself @6 :Void;
    thirdParty @7 :AnyPointer;
  }
}

struct Return {
  answerId @0 :AnswerId;
  releaseParamCaps @1 :Bool = true;
  union {
    results @2 :Payload;
    exception @3 :Exception;
    canceled @4 :Void;
    resultsSentElsewhere @5 :Void;
    takeFromOtherQuestion @6 :QuestionId;
    acceptFromThirdParty @7 :AnyPointer;
  }
}

struct Finish {
  questionId @0 :QuestionId;
  releaseResultCaps @1 :Bool = true;
}

struct Resolve {
  promiseId @0 :ExportId;
  union {
    cap @1 :CapDescriptor;
    exception @2 :Exception;
  }
}

struct Release {
  id @0 :ImportId;
  referenceCount @1 :UInt32;
}

struct MessageTarget {
  union {
    importedCap @0 :ImportId;
    promisedAnswer @1 :PromisedAnswer;
  }
}

struct Payload {
  content @0 :AnyPointer;
  capTable @1 :List(CapDescriptor);
}

struct CapDescriptor {
  union {
    none @0 :Void;
    senderHosted @1 :ExportId;
    senderPromise @2 :ExportId;
    receiverHosted @3 :ImportId;
    receiverAnswer @4 :PromisedAnswer;
    thirdPartyHosted @5 :ThirdPartyCapDescriptor;
  }
  attachedFd @6 :UInt8 = 0xff;
}

struct PromisedAnswer {
  questionId @0 :QuestionId;
  transform @1 :List(Op);
  struct Op {
    union {
      noop @0 :Void;
      getPointerField @1 :UInt16;
    }
  }
}

struct ThirdPartyCapDescriptor {
  id @0 :AnyPointer;
  vineId @1 :ExportId;
}

struct Exception {
  reason @0 :Text;
  type @3 :Type;
  obsoleteIsCallersFault @1 :Bool;
  obsoleteDurability @2 :UInt16;
  enum Type {
    failed @0;
    overloaded @1;
    disconnected @2;
    unimplemented @3;
  }
}

struct Provide {
  questionId @0 :QuestionId;
  target @1 :MessageTarget;
  recipient @2 :AnyPointer;
}

struct Accept {
  questionId @0 :QuestionId;
  provision @1 :AnyPointer;
  embargo @2 :Bool;
}

struct Join {
  questionId @0 :QuestionId;
  target @1 :AnyPointer;
}

struct Disembargo {
  target @0 :MessageTarget;
  context @1 :AnyPointer;
}
