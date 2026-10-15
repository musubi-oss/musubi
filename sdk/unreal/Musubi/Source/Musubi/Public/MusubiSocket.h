#pragma once

#include "CoreMinimal.h"
#include "Dom/JsonObject.h"
#include "IWebSocket.h"
#include "MusubiTypes.h"

DECLARE_DELEGATE_TwoParams(FMusubiReply, bool /*bOk*/, TSharedPtr<FJsonObject> /*Msg: the inner message object*/);
DECLARE_MULTICAST_DELEGATE(FMusubiSocketConnected);
DECLARE_MULTICAST_DELEGATE_TwoParams(FMusubiSocketClosed, int32 /*Code*/, const FString& /*Reason*/);
DECLARE_MULTICAST_DELEGATE_OneParam(FMusubiMatchDataEvent, const FMusubiMatchData&);
DECLARE_MULTICAST_DELEGATE_OneParam(FMusubiWorldDeltaEvent, const FMusubiWorldDelta&);
DECLARE_MULTICAST_DELEGATE_OneParam(FMusubiMatchmakerMatchedEvent, const FMusubiMatchmakerMatched&);
/** Any server message: tag (snake_case, e.g. "channel_message") + body. Fired for every message. */
DECLARE_MULTICAST_DELEGATE_TwoParams(FMusubiRawEvent, const FString& /*Tag*/, TSharedPtr<FJsonObject> /*Body*/);

/**
 * Realtime socket: JSON envelope `{cid, msg: {"<tag>": {...}}}` over `/ws?token=&format=json`.
 * Requests carry a correlation id; the first reply with the same `cid` resolves the request
 * (an `error` body resolves it with bOk = false). Events without `cid` fan out to delegates.
 */
class MUSUBI_API FMusubiSocket : public TSharedFromThis<FMusubiSocket>
{
public:
    FMusubiSocket();
    ~FMusubiSocket();

    void Connect(const FString& WsUrl);
    void Close();
    bool IsConnected() const;

    /** Send `{tag: body}` and route the correlated reply to OnReply. Returns the cid. */
    FString Request(const FString& Tag, const TSharedPtr<FJsonObject>& Body, FMusubiReply OnReply);
    /** Fire-and-forget (no correlation id). */
    void Send(const FString& Tag, const TSharedPtr<FJsonObject>& Body);

    // ---- convenience wrappers (bodies follow crates/musubi-proto/src/realtime.rs) ----
    void Ping(FMusubiReply OnReply);
    void StatusUpdate(const FString& Status);
    void StatusFollow(const TArray<FString>& AccountIds, FMusubiReply OnReply);
    void ChannelJoin(const FString& Target, int32 Type, bool bPersistence, bool bHidden, FMusubiReply OnReply);
    void ChannelMessageSend(const FString& ChannelId, const TSharedPtr<FJsonObject>& Content, FMusubiReply OnReply);
    void MatchmakerAdd(const FString& Query, int32 MinCount, int32 MaxCount, const TMap<FString, FString>& StringProps, const TMap<FString, double>& NumericProps, FMusubiReply OnReply);
    void MatchmakerRemove(const FString& Ticket, FMusubiReply OnReply);
    void MatchCreate(const FString& Name, FMusubiReply OnReply);
    void MatchJoin(const FString& MatchId, const FString& Token, FMusubiReply OnReply);
    void MatchLeave(const FString& MatchId);
    void MatchDataSend(const FString& MatchId, int64 OpCode, const TArray<uint8>& Data, bool bReliable);
    void PartyCreate(bool bOpen, int32 MaxSize, FMusubiReply OnReply);
    void PartyJoin(const FString& PartyId, FMusubiReply OnReply);
    void PartyLeave(const FString& PartyId);
    void WorldSubscribe(const FString& World, double X, double Y, double Radius);
    void WorldUnsubscribe(const FString& World);
    void WorldInput(const FString& World, const FString& EntityId, const TSharedPtr<FJsonObject>& Input);

    FMusubiSocketConnected OnConnected;
    FMusubiSocketClosed OnClosed;
    FMusubiMatchDataEvent OnMatchData;
    FMusubiWorldDeltaEvent OnWorldDelta;
    FMusubiMatchmakerMatchedEvent OnMatchmakerMatched;
    FMusubiRawEvent OnMessage;

    static FMusubiPresence ParsePresence(const TSharedPtr<FJsonObject>& O);
    static FMusubiWorldEntity ParseEntity(const TSharedPtr<FJsonObject>& O);

private:
    void HandleText(const FString& Text);
    void Dispatch(const FString& Tag, const TSharedPtr<FJsonObject>& Body);
    FString Envelope(const FString& Cid, const FString& Tag, const TSharedPtr<FJsonObject>& Body) const;

    TSharedPtr<IWebSocket> Ws;
    TMap<FString, FMusubiReply> Pending;
    uint64 NextCid = 1;
};
