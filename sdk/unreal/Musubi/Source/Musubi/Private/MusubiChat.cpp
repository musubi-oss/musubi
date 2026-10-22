#include "MusubiChat.h"
#include "MusubiSubsystem.h"

const FMusubiChannelMessage* FMusubiChannelState::FindMessage(const FString& MessageId) const
{
	return History.FindByPredicate([&](const FMusubiChannelMessage& M) { return M.message_id == MessageId; });
}

void UMusubiChat::Initialize(FSubsystemCollectionBase& Collection)
{
	Listen(EMusubiServerMessageTag::ChannelMessage, [this](const FMusubiServerMessage& M) {
		if (M.ChannelMessage.IsSet()) HandleMessage(M.ChannelMessage.GetValue());
	});
	Listen(EMusubiServerMessageTag::ChannelPresenceEvent, [this](const FMusubiServerMessage& M) {
		if (!M.ChannelPresenceEvent.IsSet()) return;
		const auto& E = M.ChannelPresenceEvent.GetValue();
		FMusubiChannelState* Ch = Channels.Find(E.channel_id);
		if (!Ch) return;
		for (const FMusubiUserPresence& P : E.leaves)
			Ch->Presences.RemoveAll([&](const FMusubiUserPresence& Q) { return Q.session_id == P.session_id; });
		for (const FMusubiUserPresence& P : E.joins)
		{
			Ch->Presences.RemoveAll([&](const FMusubiUserPresence& Q) { return Q.session_id == P.session_id; });
			Ch->Presences.Add(P);
		}
		OnPresence.Broadcast(*Ch, E);
	});
	Listen(EMusubiServerMessageTag::ChannelTyping, [this](const FMusubiServerMessage& M) {
		if (!M.ChannelTyping.IsSet()) return;
		if (const FMusubiChannelState* Ch = Channels.Find(M.ChannelTyping->channel_id)) OnTyping.Broadcast(*Ch, M.ChannelTyping->user);
	});
	Listen(EMusubiServerMessageTag::ChannelMessageReaction, [this](const FMusubiServerMessage& M) {
		if (!M.ChannelMessageReaction.IsSet()) return;
		const auto& R = M.ChannelMessageReaction.GetValue();
		FMusubiChannelState* Ch = Channels.Find(R.channel_id);
		if (!Ch) return;
		if (FMusubiChannelMessage* Msg = Ch->History.FindByPredicate([&](const FMusubiChannelMessage& X) { return X.message_id == R.message_id; }))
		{
			FMusubiReaction* Re = Msg->reactions.FindByPredicate([&](const FMusubiReaction& X) { return X.emoji == R.emoji; });
			if (!Re && R.added) { Re = &Msg->reactions.AddDefaulted_GetRef(); Re->emoji = R.emoji; }
			if (Re)
			{
				Re->count = R.count;
				if (R.account_id == Ch->Me.account_id) Re->me = R.added;
				if (Re->count == 0) Msg->reactions.RemoveAll([&](const FMusubiReaction& X) { return X.emoji == R.emoji; });
			}
		}
		OnReaction.Broadcast(*Ch, R);
	});
	Super::Initialize(Collection);
}

FMusubiChannelState& UMusubiChat::Upsert(const FMusubiServerMessageChannel& C, const FMusubiClientMessageChannelJoin& Args)
{
	// A re-join after reconnect may hand back a different id for the same target: drop the stale entry.
	for (auto It = Channels.CreateIterator(); It; ++It)
		if (It->Key != C.id && It->Value.JoinArgs.target == Args.target && It->Value.JoinArgs.kind == Args.kind) It.RemoveCurrent();
	FMusubiChannelState& Ch = Channels.FindOrAdd(C.id);
	Ch.Id = C.id;
	Ch.Kind = C.kind;
	Ch.JoinArgs = Args;
	Ch.Me = C.me;
	Ch.Presences = C.presences;
	return Ch;
}

void UMusubiChat::Push(FMusubiChannelState& Ch, const FMusubiChannelMessage& M)
{
	Ch.History.Add(M);
	if (Ch.History.Num() > HistoryCap) Ch.History.RemoveAt(0, Ch.History.Num() - HistoryCap);
}

void UMusubiChat::HandleMessage(const FMusubiChannelMessage& M)
{
	FMusubiChannelState* Ch = Channels.Find(M.channel_id);
	if (!Ch) return;
	// Node message codes: 0 = chat, 1 = update, 2 = remove (musubi-proto ChannelMessage.code).
	switch (M.code)
	{
	case 1:
		if (FMusubiChannelMessage* Old = Ch->History.FindByPredicate([&](const FMusubiChannelMessage& X) { return X.message_id == M.message_id; }))
		{
			Old->content = M.content;
			Old->update_time = M.update_time;
		}
		OnMessageUpdated.Broadcast(*Ch, M);
		return;
	case 2:
		Ch->History.RemoveAll([&](const FMusubiChannelMessage& X) { return X.message_id == M.message_id; });
		OnMessageRemoved.Broadcast(*Ch, M);
		return;
	default:
		Push(*Ch, M);
		OnMessage.Broadcast(*Ch, M);
		OnChatMessage.Broadcast(M.channel_id, M.message_id, M.username, M.content.ToText());
	}
}

void UMusubiChat::Join(const FString& Target, EMusubiChannelKind Kind, bool bPersistence, bool bHidden,
	TFunction<void(bool, const FMusubiChannelState*, const FMusubiServerEnvelope&)> OnDone)
{
	FMusubiClientMessageChannelJoin J;
	J.target = Target;
	J.kind = Kind;
	J.persistence = bPersistence;
	J.hidden = bHidden;
	FMusubiClientMessage M;
	M.Tag = EMusubiClientMessageTag::ChannelJoin;
	M.ChannelJoin = J;
	TWeakObjectPtr<UMusubiChat> Weak(this);
	Request(MoveTemp(M), FMusubiReply::CreateLambda([Weak, J, OnDone](bool bOk, const FMusubiServerEnvelope& E) {
		if (!Weak.IsValid()) return;
		if (bOk && E.Msg.Channel.IsSet())
		{
			FMusubiChannelState& Ch = Weak->Upsert(E.Msg.Channel.GetValue(), J);
			Weak->OnJoined.Broadcast(Ch);
			Weak->OnChatChannel.Broadcast(Ch.Id, true);
			if (OnDone) OnDone(true, &Ch, E);
		}
		else if (OnDone) OnDone(false, nullptr, bOk ? LocalError(EMusubiCodeTag::Internal, TEXT("unexpected reply")) : E);
	}));
}

void UMusubiChat::Leave(const FString& ChannelId)
{
	FMusubiClientMessage M;
	M.Tag = EMusubiClientMessageTag::ChannelLeave;
	FMusubiClientMessageChannelLeave L;
	L.channel_id = ChannelId;
	M.ChannelLeave = MoveTemp(L);
	Send(MoveTemp(M));
	FMusubiChannelState Gone;
	if (Channels.RemoveAndCopyValue(ChannelId, Gone))
	{
		OnLeft.Broadcast(Gone);
		OnChatChannel.Broadcast(ChannelId, false);
	}
}

void UMusubiChat::SendMessage(const FString& ChannelId, const FMusubiJson& Content, TOptional<FString> ReplyTo,
	TFunction<void(bool, const FMusubiServerMessageChannelMessageAck*, const FMusubiServerEnvelope&)> OnDone)
{
	FMusubiClientMessageChannelMessageSend S;
	S.channel_id = ChannelId;
	S.content = Content;
	S.reply_to = ReplyTo;
	FMusubiClientMessage M;
	M.Tag = EMusubiClientMessageTag::ChannelMessageSend;
	M.ChannelMessageSend = MoveTemp(S);
	TWeakObjectPtr<UMusubiChat> Weak(this);
	Request(MoveTemp(M), FMusubiReply::CreateLambda([Weak, ChannelId, Content, ReplyTo, OnDone](bool bOk, const FMusubiServerEnvelope& E) {
		if (bOk && E.Msg.ChannelMessageAck.IsSet())
		{
			const auto& A = E.Msg.ChannelMessageAck.GetValue();
			if (Weak.IsValid())
			{
				// Our own message is not echoed back on `channel_message`; materialise it from the ack.
				if (FMusubiChannelState* Ch = Weak->Channels.Find(ChannelId))
				{
					if (!Ch->FindMessage(A.message_id))
					{
						FMusubiChannelMessage Mine;
						Mine.channel_id = A.channel_id;
						Mine.message_id = A.message_id;
						Mine.kind = Ch->Kind;
						Mine.code = A.code;
						Mine.sender_id = Ch->Me.account_id;
						Mine.username = Ch->Me.username;
						Mine.content = Content;
						Mine.create_time = A.create_time;
						Mine.update_time = A.update_time;
						Mine.persistent = A.persistent;
						Mine.reply_to = ReplyTo;
						Weak->Push(*Ch, Mine);
						Weak->OnMessage.Broadcast(*Ch, Mine);
					}
				}
			}
			if (OnDone) OnDone(true, &A, E);
		}
		else if (OnDone) OnDone(false, nullptr, bOk ? LocalError(EMusubiCodeTag::Internal, TEXT("unexpected reply")) : E);
	}));
}

void UMusubiChat::UpdateMessage(const FString& ChannelId, const FString& MessageId, const FMusubiJson& Content, FMusubiReply OnDone)
{
	FMusubiClientMessageChannelMessageUpdate U;
	U.channel_id = ChannelId;
	U.message_id = MessageId;
	U.content = Content;
	FMusubiClientMessage M;
	M.Tag = EMusubiClientMessageTag::ChannelMessageUpdate;
	M.ChannelMessageUpdate = MoveTemp(U);
	Request(MoveTemp(M), MoveTemp(OnDone));
}

void UMusubiChat::RemoveMessage(const FString& ChannelId, const FString& MessageId, FMusubiReply OnDone)
{
	FMusubiClientMessageChannelMessageRemove R;
	R.channel_id = ChannelId;
	R.message_id = MessageId;
	FMusubiClientMessage M;
	M.Tag = EMusubiClientMessageTag::ChannelMessageRemove;
	M.ChannelMessageRemove = MoveTemp(R);
	Request(MoveTemp(M), MoveTemp(OnDone));
}

void UMusubiChat::Typing(const FString& ChannelId)
{
	const double Now = FPlatformTime::Seconds();
	double& Last = LastTyping.FindOrAdd(ChannelId, -1e9);
	if (Now - Last < TypingIntervalSec) return;
	Last = Now;
	FMusubiClientMessage M;
	M.Tag = EMusubiClientMessageTag::ChannelTyping;
	FMusubiClientMessageChannelTyping T;
	T.channel_id = ChannelId;
	M.ChannelTyping = MoveTemp(T);
	Send(MoveTemp(M));
}

void UMusubiChat::OnResubscribe()
{
	TArray<FMusubiClientMessageChannelJoin> Again;
	for (const auto& P : Channels) Again.Add(P.Value.JoinArgs);
	for (const FMusubiClientMessageChannelJoin& J : Again) Join(J.target, J.kind, J.persistence, J.hidden);
}

void UMusubiChat::OnDisconnected()
{
	for (auto& P : Channels) P.Value.Presences.Empty();
	LastTyping.Empty();
}

// ---- Blueprint ----


void UMusubiChat::JoinRoom(const FString& Room, bool bPersistence, bool bHidden, FMusubiCallDone OnDone)
{
	Join(Room, EMusubiChannelKind::Room, bPersistence, bHidden, [OnDone](bool bOk, const FMusubiChannelState*, const FMusubiServerEnvelope& E) {
		OnDone.ExecuteIfBound(FMusubiCallResult::From(bOk, E));
	});
}

void UMusubiChat::JoinDirect(const FString& AccountId, bool bPersistence, FMusubiCallDone OnDone)
{
	Join(AccountId, EMusubiChannelKind::Direct, bPersistence, false, [OnDone](bool bOk, const FMusubiChannelState*, const FMusubiServerEnvelope& E) {
		OnDone.ExecuteIfBound(FMusubiCallResult::From(bOk, E));
	});
}

void UMusubiChat::JoinGroup(const FString& GroupId, bool bPersistence, bool bHidden, FMusubiCallDone OnDone)
{
	Join(GroupId, EMusubiChannelKind::Group, bPersistence, bHidden, [OnDone](bool bOk, const FMusubiChannelState*, const FMusubiServerEnvelope& E) {
		OnDone.ExecuteIfBound(FMusubiCallResult::From(bOk, E));
	});
}

void UMusubiChat::LeaveChannel(const FString& ChannelId) { Leave(ChannelId); }

void UMusubiChat::SendText(const FString& ChannelId, const FString& Text, FMusubiCallDone OnDone)
{
	TSharedRef<FJsonObject> O = MakeShared<FJsonObject>();
	O->SetStringField(TEXT("text"), Text);
	FMusubiJson Content;
	Content.Value = MakeShared<FJsonValueObject>(O);
	SendMessage(ChannelId, Content, {}, [OnDone](bool bOk, const FMusubiServerMessageChannelMessageAck*, const FMusubiServerEnvelope& E) {
		OnDone.ExecuteIfBound(FMusubiCallResult::From(bOk, E));
	});
}

void UMusubiChat::SendJson(const FString& ChannelId, const FString& ContentJson, const FString& ReplyToMessageId, FMusubiCallDone OnDone)
{
	SendMessage(ChannelId, FMusubiJson::FromText(ContentJson), ReplyToMessageId.IsEmpty() ? TOptional<FString>() : TOptional<FString>(ReplyToMessageId),
		[OnDone](bool bOk, const FMusubiServerMessageChannelMessageAck*, const FMusubiServerEnvelope& E) { OnDone.ExecuteIfBound(FMusubiCallResult::From(bOk, E)); });
}

void UMusubiChat::SendTyping(const FString& ChannelId) { Typing(ChannelId); }
