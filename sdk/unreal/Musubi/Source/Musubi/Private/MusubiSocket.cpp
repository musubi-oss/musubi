#include "MusubiSocket.h"
#include "MusubiModule.h"
#include "Misc/Base64.h"
#include "Serialization/JsonReader.h"
#include "Serialization/JsonSerializer.h"
#include "Serialization/JsonWriter.h"
#include "WebSocketsModule.h"

FMusubiSocket::FMusubiSocket() {}

FMusubiSocket::~FMusubiSocket()
{
    Close();
}

bool FMusubiSocket::IsConnected() const
{
    return Ws.IsValid() && Ws->IsConnected();
}

void FMusubiSocket::Connect(const FString& WsUrl)
{
    Close();
    Ws = FWebSocketsModule::Get().CreateWebSocket(WsUrl, TEXT(""));
    TWeakPtr<FMusubiSocket> Weak = AsShared();
    Ws->OnConnected().AddLambda([Weak]()
    {
        if (TSharedPtr<FMusubiSocket> S = Weak.Pin()) S->OnConnected.Broadcast();
    });
    Ws->OnConnectionError().AddLambda([Weak](const FString& Err)
    {
        UE_LOG(LogMusubi, Warning, TEXT("socket connection error: %s"), *Err);
        if (TSharedPtr<FMusubiSocket> S = Weak.Pin()) S->OnClosed.Broadcast(0, Err);
    });
    Ws->OnClosed().AddLambda([Weak](int32 Code, const FString& Reason, bool)
    {
        if (TSharedPtr<FMusubiSocket> S = Weak.Pin())
        {
            for (auto& P : S->Pending) P.Value.ExecuteIfBound(false, nullptr);
            S->Pending.Empty();
            S->OnClosed.Broadcast(Code, Reason);
        }
    });
    Ws->OnMessage().AddLambda([Weak](const FString& Text)
    {
        if (TSharedPtr<FMusubiSocket> S = Weak.Pin()) S->HandleText(Text);
    });
    Ws->Connect();
}

void FMusubiSocket::Close()
{
    if (Ws.IsValid())
    {
        if (Ws->IsConnected()) Ws->Close();
        Ws.Reset();
    }
}

FString FMusubiSocket::Envelope(const FString& Cid, const FString& Tag, const TSharedPtr<FJsonObject>& Body) const
{
    TSharedRef<FJsonObject> Env = MakeShared<FJsonObject>();
    if (!Cid.IsEmpty()) Env->SetStringField(TEXT("cid"), Cid);
    TSharedRef<FJsonObject> Msg = MakeShared<FJsonObject>();
    if (Body.IsValid()) Msg->SetObjectField(Tag, Body);
    else Msg->SetObjectField(Tag, MakeShared<FJsonObject>());
    Env->SetObjectField(TEXT("msg"), Msg);
    FString Out;
    TSharedRef<TJsonWriter<>> W = TJsonWriterFactory<>::Create(&Out);
    FJsonSerializer::Serialize(Env, W);
    return Out;
}

FString FMusubiSocket::Request(const FString& Tag, const TSharedPtr<FJsonObject>& Body, FMusubiReply OnReply)
{
    const FString Cid = FString::Printf(TEXT("c%llu"), NextCid++);
    if (!IsConnected())
    {
        OnReply.ExecuteIfBound(false, nullptr);
        return Cid;
    }
    Pending.Add(Cid, OnReply);
    Ws->Send(Envelope(Cid, Tag, Body));
    return Cid;
}

void FMusubiSocket::Send(const FString& Tag, const TSharedPtr<FJsonObject>& Body)
{
    if (IsConnected()) Ws->Send(Envelope(TEXT(""), Tag, Body));
}

void FMusubiSocket::HandleText(const FString& Text)
{
    TSharedPtr<FJsonObject> Env;
    TSharedRef<TJsonReader<>> R = TJsonReaderFactory<>::Create(Text);
    if (!FJsonSerializer::Deserialize(R, Env) || !Env.IsValid()) return;
    FString Cid;
    Env->TryGetStringField(TEXT("cid"), Cid);
    // Externally tagged enum: `msg` is either a string (unit variant, e.g. "pong") or `{tag: body}`.
    FString Tag;
    TSharedPtr<FJsonObject> Body;
    if (Env->HasTypedField<EJson::String>(TEXT("msg")))
    {
        Tag = Env->GetStringField(TEXT("msg"));
        Body = MakeShared<FJsonObject>();
    }
    else
    {
        const TSharedPtr<FJsonObject>* Msg = nullptr;
        if (!Env->TryGetObjectField(TEXT("msg"), Msg) || !Msg || !(*Msg).IsValid() || (*Msg)->Values.Num() == 0) return;
        for (const auto& KV : (*Msg)->Values)
        {
            Tag = KV.Key;
            const TSharedPtr<FJsonObject>* B = nullptr;
            if (KV.Value->TryGetObject(B) && B) Body = *B;
            else Body = MakeShared<FJsonObject>();
            break;
        }
    }
    if (!Cid.IsEmpty())
    {
        FMusubiReply Reply;
        if (Pending.RemoveAndCopyValue(Cid, Reply))
        {
            Reply.ExecuteIfBound(Tag != TEXT("error"), Body);
            if (Tag == TEXT("error")) return;
        }
    }
    Dispatch(Tag, Body);
}

FMusubiPresence FMusubiSocket::ParsePresence(const TSharedPtr<FJsonObject>& O)
{
    FMusubiPresence P;
    if (!O.IsValid()) return P;
    O->TryGetStringField(TEXT("account_id"), P.AccountId);
    O->TryGetStringField(TEXT("session_id"), P.SessionId);
    O->TryGetStringField(TEXT("username"), P.Username);
    O->TryGetStringField(TEXT("status"), P.Status);
    return P;
}

FMusubiWorldEntity FMusubiSocket::ParseEntity(const TSharedPtr<FJsonObject>& O)
{
    FMusubiWorldEntity E;
    if (!O.IsValid()) return E;
    O->TryGetStringField(TEXT("id"), E.Id);
    O->TryGetNumberField(TEXT("x"), E.X);
    O->TryGetNumberField(TEXT("y"), E.Y);
    const TArray<TSharedPtr<FJsonValue>>* Comps = nullptr;
    if (O->TryGetArrayField(TEXT("components"), Comps) && Comps)
    {
        for (const auto& V : *Comps)
        {
            const TSharedPtr<FJsonObject>* C = nullptr;
            if (!V->TryGetObject(C) || !C) continue;
            FMusubiWorldComponent Comp;
            (*C)->TryGetStringField(TEXT("name"), Comp.Name);
            double Ver = 0;
            (*C)->TryGetNumberField(TEXT("version"), Ver);
            Comp.Version = static_cast<int64>(Ver);
            TSharedPtr<FJsonValue> Val = (*C)->TryGetField(TEXT("value"));
            if (Val.IsValid())
            {
                TSharedRef<TJsonWriter<>> W = TJsonWriterFactory<>::Create(&Comp.ValueJson);
                FJsonSerializer::Serialize(Val, TEXT(""), W, false);
            }
            E.Components.Add(Comp);
        }
    }
    return E;
}

void FMusubiSocket::Dispatch(const FString& Tag, const TSharedPtr<FJsonObject>& Body)
{
    OnMessage.Broadcast(Tag, Body);
    if (!Body.IsValid()) return;
    if (Tag == TEXT("match_data"))
    {
        FMusubiMatchData D;
        Body->TryGetStringField(TEXT("match_id"), D.MatchId);
        const TSharedPtr<FJsonObject>* P = nullptr;
        if (Body->TryGetObjectField(TEXT("presence"), P) && P) D.Sender = ParsePresence(*P);
        double Op = 0;
        Body->TryGetNumberField(TEXT("op_code"), Op);
        D.OpCode = static_cast<int64>(Op);
        Body->TryGetBoolField(TEXT("reliable"), D.bReliable);
        const TArray<TSharedPtr<FJsonValue>>* Bytes = nullptr;
        if (Body->TryGetArrayField(TEXT("data"), Bytes) && Bytes)
        {
            D.Data.Reserve(Bytes->Num());
            for (const auto& B : *Bytes) D.Data.Add(static_cast<uint8>(B->AsNumber()));
        }
        OnMatchData.Broadcast(D);
    }
    else if (Tag == TEXT("world_delta"))
    {
        FMusubiWorldDelta D;
        Body->TryGetStringField(TEXT("world"), D.World);
        double T = 0;
        Body->TryGetNumberField(TEXT("tick"), T);
        D.Tick = static_cast<int64>(T);
        auto Fill = [&Body](const TCHAR* Field, TArray<FMusubiWorldEntity>& Out)
        {
            const TArray<TSharedPtr<FJsonValue>>* Arr = nullptr;
            if (!Body->TryGetArrayField(Field, Arr) || !Arr) return;
            for (const auto& V : *Arr)
            {
                const TSharedPtr<FJsonObject>* O = nullptr;
                if (V->TryGetObject(O) && O) Out.Add(ParseEntity(*O));
            }
        };
        Fill(TEXT("spawned"), D.Spawned);
        Fill(TEXT("updated"), D.Updated);
        Body->TryGetStringArrayField(TEXT("despawned"), D.Despawned);
        OnWorldDelta.Broadcast(D);
    }
    else if (Tag == TEXT("matchmaker_matched"))
    {
        FMusubiMatchmakerMatched M;
        Body->TryGetStringField(TEXT("ticket"), M.Ticket);
        Body->TryGetStringField(TEXT("token"), M.Token);
        const TArray<TSharedPtr<FJsonValue>>* Users = nullptr;
        if (Body->TryGetArrayField(TEXT("users"), Users) && Users)
        {
            for (const auto& U : *Users)
            {
                const TSharedPtr<FJsonObject>* O = nullptr;
                const TSharedPtr<FJsonObject>* P = nullptr;
                if (U->TryGetObject(O) && O && (*O)->TryGetObjectField(TEXT("presence"), P) && P) M.Users.Add(ParsePresence(*P));
            }
        }
        const TSharedPtr<FJsonObject>* S = nullptr;
        if (Body->TryGetObjectField(TEXT("server"), S) && S && (*S).IsValid())
        {
            M.bHasServer = true;
            (*S)->TryGetStringField(TEXT("server_id"), M.Server.ServerId);
            (*S)->TryGetStringField(TEXT("address"), M.Server.Address);
            (*S)->TryGetNumberField(TEXT("port"), M.Server.Port);
            (*S)->TryGetStringField(TEXT("connect_token"), M.Server.ConnectToken);
        }
        OnMatchmakerMatched.Broadcast(M);
    }
}

// ---- wrappers ----

void FMusubiSocket::Ping(FMusubiReply OnReply) { Request(TEXT("ping"), nullptr, OnReply); }

void FMusubiSocket::StatusUpdate(const FString& Status)
{
    TSharedPtr<FJsonObject> B = MakeShared<FJsonObject>();
    if (Status.IsEmpty()) B->SetField(TEXT("status"), MakeShared<FJsonValueNull>());
    else B->SetStringField(TEXT("status"), Status);
    Send(TEXT("status_update"), B);
}

void FMusubiSocket::StatusFollow(const TArray<FString>& AccountIds, FMusubiReply OnReply)
{
    TSharedPtr<FJsonObject> B = MakeShared<FJsonObject>();
    TArray<TSharedPtr<FJsonValue>> Ids;
    for (const FString& Id : AccountIds) Ids.Add(MakeShared<FJsonValueString>(Id));
    B->SetArrayField(TEXT("account_ids"), Ids);
    Request(TEXT("status_follow"), B, OnReply);
}

void FMusubiSocket::ChannelJoin(const FString& Target, int32 Type, bool bPersistence, bool bHidden, FMusubiReply OnReply)
{
    TSharedPtr<FJsonObject> B = MakeShared<FJsonObject>();
    B->SetStringField(TEXT("target"), Target);
    B->SetNumberField(TEXT("type"), Type);
    B->SetBoolField(TEXT("persistence"), bPersistence);
    B->SetBoolField(TEXT("hidden"), bHidden);
    Request(TEXT("channel_join"), B, OnReply);
}

void FMusubiSocket::ChannelMessageSend(const FString& ChannelId, const TSharedPtr<FJsonObject>& Content, FMusubiReply OnReply)
{
    TSharedPtr<FJsonObject> B = MakeShared<FJsonObject>();
    B->SetStringField(TEXT("channel_id"), ChannelId);
    B->SetObjectField(TEXT("content"), Content.IsValid() ? Content : MakeShared<FJsonObject>());
    Request(TEXT("channel_message_send"), B, OnReply);
}

void FMusubiSocket::MatchmakerAdd(const FString& Query, int32 MinCount, int32 MaxCount, const TMap<FString, FString>& StringProps, const TMap<FString, double>& NumericProps, FMusubiReply OnReply)
{
    TSharedPtr<FJsonObject> B = MakeShared<FJsonObject>();
    B->SetStringField(TEXT("query"), Query);
    B->SetNumberField(TEXT("min_count"), MinCount);
    B->SetNumberField(TEXT("max_count"), MaxCount);
    B->SetNumberField(TEXT("count_multiple"), 1);
    TSharedPtr<FJsonObject> SP = MakeShared<FJsonObject>();
    for (const auto& KV : StringProps) SP->SetStringField(KV.Key, KV.Value);
    TSharedPtr<FJsonObject> NP = MakeShared<FJsonObject>();
    for (const auto& KV : NumericProps) NP->SetNumberField(KV.Key, KV.Value);
    B->SetObjectField(TEXT("string_properties"), SP);
    B->SetObjectField(TEXT("numeric_properties"), NP);
    Request(TEXT("matchmaker_add"), B, OnReply);
}

void FMusubiSocket::MatchmakerRemove(const FString& Ticket, FMusubiReply OnReply)
{
    TSharedPtr<FJsonObject> B = MakeShared<FJsonObject>();
    B->SetStringField(TEXT("ticket"), Ticket);
    Request(TEXT("matchmaker_remove"), B, OnReply);
}

void FMusubiSocket::MatchCreate(const FString& Name, FMusubiReply OnReply)
{
    TSharedPtr<FJsonObject> B = MakeShared<FJsonObject>();
    if (!Name.IsEmpty()) B->SetStringField(TEXT("name"), Name);
    Request(TEXT("match_create"), B, OnReply);
}

void FMusubiSocket::MatchJoin(const FString& MatchId, const FString& Token, FMusubiReply OnReply)
{
    TSharedPtr<FJsonObject> B = MakeShared<FJsonObject>();
    if (!MatchId.IsEmpty()) B->SetStringField(TEXT("match_id"), MatchId);
    if (!Token.IsEmpty()) B->SetStringField(TEXT("token"), Token);
    B->SetObjectField(TEXT("metadata"), MakeShared<FJsonObject>());
    Request(TEXT("match_join"), B, OnReply);
}

void FMusubiSocket::MatchLeave(const FString& MatchId)
{
    TSharedPtr<FJsonObject> B = MakeShared<FJsonObject>();
    B->SetStringField(TEXT("match_id"), MatchId);
    Send(TEXT("match_leave"), B);
}

void FMusubiSocket::MatchDataSend(const FString& MatchId, int64 OpCode, const TArray<uint8>& Data, bool bReliable)
{
    TSharedPtr<FJsonObject> B = MakeShared<FJsonObject>();
    B->SetStringField(TEXT("match_id"), MatchId);
    B->SetNumberField(TEXT("op_code"), static_cast<double>(OpCode));
    TArray<TSharedPtr<FJsonValue>> Bytes;
    Bytes.Reserve(Data.Num());
    for (uint8 Byte : Data) Bytes.Add(MakeShared<FJsonValueNumber>(Byte));
    B->SetArrayField(TEXT("data"), Bytes);
    B->SetArrayField(TEXT("presences"), {});
    B->SetBoolField(TEXT("reliable"), bReliable);
    Send(TEXT("match_data_send"), B);
}

void FMusubiSocket::PartyCreate(bool bOpen, int32 MaxSize, FMusubiReply OnReply)
{
    TSharedPtr<FJsonObject> B = MakeShared<FJsonObject>();
    B->SetBoolField(TEXT("open"), bOpen);
    B->SetNumberField(TEXT("max_size"), MaxSize);
    Request(TEXT("party_create"), B, OnReply);
}

void FMusubiSocket::PartyJoin(const FString& PartyId, FMusubiReply OnReply)
{
    TSharedPtr<FJsonObject> B = MakeShared<FJsonObject>();
    B->SetStringField(TEXT("party_id"), PartyId);
    Request(TEXT("party_join"), B, OnReply);
}

void FMusubiSocket::PartyLeave(const FString& PartyId)
{
    TSharedPtr<FJsonObject> B = MakeShared<FJsonObject>();
    B->SetStringField(TEXT("party_id"), PartyId);
    Send(TEXT("party_leave"), B);
}

void FMusubiSocket::WorldSubscribe(const FString& World, double X, double Y, double Radius)
{
    TSharedPtr<FJsonObject> B = MakeShared<FJsonObject>();
    B->SetStringField(TEXT("world"), World);
    B->SetNumberField(TEXT("x"), X);
    B->SetNumberField(TEXT("y"), Y);
    B->SetNumberField(TEXT("radius"), Radius);
    Send(TEXT("world_subscribe"), B);
}

void FMusubiSocket::WorldUnsubscribe(const FString& World)
{
    TSharedPtr<FJsonObject> B = MakeShared<FJsonObject>();
    B->SetStringField(TEXT("world"), World);
    Send(TEXT("world_unsubscribe"), B);
}

void FMusubiSocket::WorldInput(const FString& World, const FString& EntityId, const TSharedPtr<FJsonObject>& Input)
{
    TSharedPtr<FJsonObject> B = MakeShared<FJsonObject>();
    B->SetStringField(TEXT("world"), World);
    B->SetStringField(TEXT("entity"), EntityId);
    B->SetObjectField(TEXT("input"), Input.IsValid() ? Input : MakeShared<FJsonObject>());
    Send(TEXT("world_input"), B);
}
