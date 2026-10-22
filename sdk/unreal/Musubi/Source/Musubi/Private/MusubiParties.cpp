#include "MusubiParties.h"

void UMusubiParties::Initialize(FSubsystemCollectionBase& Collection)
{
	Listen(EMusubiServerMessageTag::Party, [this](const FMusubiServerMessage& M) {
		// Unsolicited `party` = full refresh (after update/accept/remove); solicited ones arrive via replies.
		if (M.Party.IsSet() && Current.IsSet() && M.Party->party_id == Current->party_id) SetParty(M.Party.GetValue(), false);
	});
	Listen(EMusubiServerMessageTag::PartyPresenceEvent, [this](const FMusubiServerMessage& M) {
		if (!M.PartyPresenceEvent.IsSet() || !Current.IsSet() || M.PartyPresenceEvent->party_id != Current->party_id) return;
		const auto& E = M.PartyPresenceEvent.GetValue();
		for (const FMusubiUserPresence& P : E.leaves)
			Current->presences.RemoveAll([&](const FMusubiUserPresence& Q) { return Q.session_id == P.session_id; });
		for (const FMusubiUserPresence& P : E.joins)
		{
			Current->presences.RemoveAll([&](const FMusubiUserPresence& Q) { return Q.session_id == P.session_id; });
			Current->presences.Add(P);
		}
		OnPresence.Broadcast(E);
		OnUpdated.Broadcast(Current.GetValue());
	});
	Listen(EMusubiServerMessageTag::PartyJoinRequest, [this](const FMusubiServerMessage& M) {
		if (!M.PartyJoinRequest.IsSet() || !Current.IsSet() || M.PartyJoinRequest->party_id != Current->party_id) return;
		for (const FMusubiUserPresence& P : M.PartyJoinRequest->presences)
		{
			JoinRequests.RemoveAll([&](const FMusubiUserPresence& Q) { return Q.session_id == P.session_id; });
			JoinRequests.Add(P);
		}
		OnJoinRequest.Broadcast(M.PartyJoinRequest.GetValue());
	});
	Listen(EMusubiServerMessageTag::PartyLeader, [this](const FMusubiServerMessage& M) {
		if (!M.PartyLeader.IsSet() || !Current.IsSet() || M.PartyLeader->party_id != Current->party_id) return;
		Current->leader = M.PartyLeader->presence;
		OnLeader.Broadcast(M.PartyLeader.GetValue());
		OnUpdated.Broadcast(Current.GetValue());
	});
	Listen(EMusubiServerMessageTag::PartyData, [this](const FMusubiServerMessage& M) {
		if (!M.PartyData.IsSet()) return;
		OnData.Broadcast(M.PartyData.GetValue());
		OnPartyData.Broadcast(M.PartyData->party_id, M.PartyData->op_code, M.PartyData->data);
	});
	Listen(EMusubiServerMessageTag::PartyClose, [this](const FMusubiServerMessage& M) {
		if (M.PartyClose.IsSet() && Current.IsSet() && M.PartyClose->party_id == Current->party_id) ClearParty(TEXT("closed"));
	});
	Listen(EMusubiServerMessageTag::PartyInvite, [this](const FMusubiServerMessage& M) {
		if (!M.PartyInvite.IsSet()) return;
		OnInvite.Broadcast(M.PartyInvite.GetValue());
		OnPartyInvite.Broadcast(M.PartyInvite->party_id, M.PartyInvite->from.account_id.ToString(), M.PartyInvite->from.username);
	});
	Super::Initialize(Collection);
}

void UMusubiParties::SetParty(const FMusubiServerMessageParty& P, bool bFresh)
{
	const bool bWasIn = Current.IsSet();
	Current = P;
	RejoinId = P.party_id;
	if (bFresh) JoinRequests.Empty();
	// Accepted requesters are members now.
	JoinRequests.RemoveAll([&](const FMusubiUserPresence& R) {
		return P.presences.ContainsByPredicate([&](const FMusubiUserPresence& Q) { return Q.session_id == R.session_id; });
	});
	if (!bWasIn || bFresh) { OnJoined.Broadcast(P); OnPartyChanged.Broadcast(P.party_id, true); }
	else OnUpdated.Broadcast(P);
}

void UMusubiParties::ClearParty(const FString& Why)
{
	if (!Current.IsSet()) return;
	const FString Id = Current->party_id;
	Current.Reset();
	JoinRequests.Empty();
	RejoinId.Empty();
	OnLeft.Broadcast(Id);
	OnPartyChanged.Broadcast(Id, false);
}

FMusubiReply UMusubiParties::PartyReply(FPartyDone OnDone)
{
	TWeakObjectPtr<UMusubiParties> Weak(this);
	return FMusubiReply::CreateLambda([Weak, OnDone](bool bOk, const FMusubiServerEnvelope& E) {
		if (bOk && E.Msg.Party.IsSet())
		{
			if (Weak.IsValid()) Weak->SetParty(E.Msg.Party.GetValue(), true);
			if (OnDone) OnDone(true, Weak.IsValid() ? Weak->Party() : &E.Msg.Party.GetValue(), E);
		}
		else if (OnDone) OnDone(false, nullptr, bOk ? LocalError(EMusubiCodeTag::Internal, TEXT("unexpected reply")) : E);
	});
}

void UMusubiParties::Create(bool bOpen, uint32 MaxSize, TOptional<FString> Label, bool bHidden, FPartyDone OnDone)
{
	FMusubiClientMessagePartyCreate C;
	C.open = bOpen; C.max_size = MaxSize; C.label = Label; C.hidden = bHidden;
	Request(WithParty(EMusubiClientMessageTag::PartyCreate, &FMusubiClientMessage::PartyCreate, MoveTemp(C)), PartyReply(OnDone));
}

void UMusubiParties::Join(const FString& PartyId, FPartyDone OnDone)
{
	FMusubiClientMessagePartyJoin J;
	J.party_id = PartyId;
	Request(WithParty(EMusubiClientMessageTag::PartyJoin, &FMusubiClientMessage::PartyJoin, MoveTemp(J)), PartyReply(OnDone));
}

void UMusubiParties::Leave()
{
	if (!Current.IsSet()) return;
	FMusubiClientMessagePartyLeave L;
	L.party_id = Current->party_id;
	Send(WithParty(EMusubiClientMessageTag::PartyLeave, &FMusubiClientMessage::PartyLeave, MoveTemp(L)));
	ClearParty(TEXT("left"));
}

void UMusubiParties::Close()
{
	if (!Current.IsSet()) return;
	FMusubiClientMessagePartyClose C;
	C.party_id = Current->party_id;
	Send(WithParty(EMusubiClientMessageTag::PartyClose, &FMusubiClientMessage::PartyClose, MoveTemp(C)));
	ClearParty(TEXT("closed"));
}

void UMusubiParties::Update(TOptional<FString> Label, TOptional<bool> bOpen, TOptional<bool> bHidden, TOptional<uint32> MaxSize, FMusubiReply OnDone)
{
	if (!Current.IsSet()) { OnDone.ExecuteIfBound(false, LocalError(EMusubiCodeTag::InvalidArgument, TEXT("not in a party"))); return; }
	FMusubiClientMessagePartyUpdate U;
	U.party_id = Current->party_id; U.label = Label; U.open = bOpen; U.hidden = bHidden; U.max_size = MaxSize;
	Request(WithParty(EMusubiClientMessageTag::PartyUpdate, &FMusubiClientMessage::PartyUpdate, MoveTemp(U)), MoveTemp(OnDone));
}

void UMusubiParties::Promote(const FMusubiUserPresence& Member, FMusubiReply OnDone)
{
	if (!Current.IsSet()) { OnDone.ExecuteIfBound(false, LocalError(EMusubiCodeTag::InvalidArgument, TEXT("not in a party"))); return; }
	FMusubiClientMessagePartyPromote P;
	P.party_id = Current->party_id; P.presence = Member;
	Request(WithParty(EMusubiClientMessageTag::PartyPromote, &FMusubiClientMessage::PartyPromote, MoveTemp(P)), MoveTemp(OnDone));
}

void UMusubiParties::Accept(const FMusubiUserPresence& Requester, FMusubiReply OnDone)
{
	if (!Current.IsSet()) { OnDone.ExecuteIfBound(false, LocalError(EMusubiCodeTag::InvalidArgument, TEXT("not in a party"))); return; }
	FMusubiClientMessagePartyAccept A;
	A.party_id = Current->party_id; A.presence = Requester;
	Request(WithParty(EMusubiClientMessageTag::PartyAccept, &FMusubiClientMessage::PartyAccept, MoveTemp(A)), MoveTemp(OnDone));
}

void UMusubiParties::Remove(const FMusubiUserPresence& Member, FMusubiReply OnDone)
{
	if (!Current.IsSet()) { OnDone.ExecuteIfBound(false, LocalError(EMusubiCodeTag::InvalidArgument, TEXT("not in a party"))); return; }
	FMusubiClientMessagePartyRemove R;
	R.party_id = Current->party_id; R.presence = Member;
	Request(WithParty(EMusubiClientMessageTag::PartyRemove, &FMusubiClientMessage::PartyRemove, MoveTemp(R)), MoveTemp(OnDone));
}

void UMusubiParties::Invite(const FMusubiUuid& AccountId, FMusubiReply OnDone)
{
	if (!Current.IsSet()) { OnDone.ExecuteIfBound(false, LocalError(EMusubiCodeTag::InvalidArgument, TEXT("not in a party"))); return; }
	FMusubiClientMessagePartyInvite I;
	I.party_id = Current->party_id; I.account_id = AccountId;
	Request(WithParty(EMusubiClientMessageTag::PartyInvite, &FMusubiClientMessage::PartyInvite, MoveTemp(I)), MoveTemp(OnDone));
}

void UMusubiParties::ListJoinRequests(TFunction<void(bool, const TArray<FMusubiUserPresence>&)> OnDone)
{
	if (!Current.IsSet()) { if (OnDone) OnDone(false, {}); return; }
	FMusubiClientMessagePartyJoinRequestList L;
	L.party_id = Current->party_id;
	TWeakObjectPtr<UMusubiParties> Weak(this);
	Request(WithParty(EMusubiClientMessageTag::PartyJoinRequestList, &FMusubiClientMessage::PartyJoinRequestList, MoveTemp(L)),
		Expect<FMusubiServerMessagePartyJoinRequest>(EMusubiServerMessageTag::PartyJoinRequest, &FMusubiServerMessage::PartyJoinRequest,
			[Weak, OnDone](const FMusubiServerMessagePartyJoinRequest& R) {
				if (Weak.IsValid()) Weak->JoinRequests = R.presences;
				if (OnDone) OnDone(true, R.presences);
			},
			[OnDone](const FMusubiServerEnvelope&) { if (OnDone) OnDone(false, {}); }));
}

void UMusubiParties::SendData(int64 OpCode, const TArray<uint8>& Data)
{
	if (!Current.IsSet()) return;
	FMusubiClientMessagePartyDataSend D;
	D.party_id = Current->party_id; D.op_code = OpCode; D.data = Data;
	Send(WithParty(EMusubiClientMessageTag::PartyDataSend, &FMusubiClientMessage::PartyDataSend, MoveTemp(D)));
}

void UMusubiParties::List(TOptional<FString> Label, TOptional<FString> LabelPrefix, TOptional<bool> bOpen, uint32 Limit,
	TFunction<void(bool, const TArray<FMusubiPartyListing>&)> OnDone)
{
	FMusubiClientMessagePartyList L;
	L.label = Label; L.label_prefix = LabelPrefix; L.open = bOpen; L.limit = Limit;
	Request(WithParty(EMusubiClientMessageTag::PartyList, &FMusubiClientMessage::PartyList, MoveTemp(L)),
		Expect<FMusubiServerMessagePartyList>(EMusubiServerMessageTag::PartyList, &FMusubiServerMessage::PartyList,
			[OnDone](const FMusubiServerMessagePartyList& R) { if (OnDone) OnDone(true, R.parties); },
			[OnDone](const FMusubiServerEnvelope&) { if (OnDone) OnDone(false, {}); }));
}

void UMusubiParties::OnResubscribe()
{
	if (RejoinId.IsEmpty()) return;
	TWeakObjectPtr<UMusubiParties> Weak(this);
	const FString Id = RejoinId;
	Join(Id, [Weak](bool bOk, const FMusubiServerMessageParty*, const FMusubiServerEnvelope&) {
		// The party may have been closed while we were away: surface that as a leave.
		if (!bOk && Weak.IsValid()) Weak->ClearParty(TEXT("gone after reconnect"));
	});
}

void UMusubiParties::OnDisconnected()
{
	// Keep `Current` so UI stays stable; `RejoinId` drives the reconnect. Requests are stale though.
	JoinRequests.Empty();
}

// ---- Blueprint ----

void UMusubiParties::CreateParty(bool bOpen, int32 MaxSize, const FString& Label, bool bHidden, FMusubiCallDone OnDone)
{
	Create(bOpen, (uint32)FMath::Max(1, MaxSize), Label.IsEmpty() ? TOptional<FString>() : TOptional<FString>(Label), bHidden,
		[OnDone](bool bOk, const FMusubiServerMessageParty*, const FMusubiServerEnvelope& E) { OnDone.ExecuteIfBound(FMusubiCallResult::From(bOk, E)); });
}

void UMusubiParties::JoinParty(const FString& PartyId, FMusubiCallDone OnDone)
{
	Join(PartyId, [OnDone](bool bOk, const FMusubiServerMessageParty*, const FMusubiServerEnvelope& E) { OnDone.ExecuteIfBound(FMusubiCallResult::From(bOk, E)); });
}

void UMusubiParties::LeaveParty() { Leave(); }
void UMusubiParties::SendPartyData(int64 OpCode, const TArray<uint8>& Data) { SendData(OpCode, Data); }

void UMusubiParties::InviteToParty(const FString& AccountId)
{
	FMusubiUuid U;
	if (FMusubiUuid::Parse(AccountId, U)) Invite(U);
}

TArray<FString> UMusubiParties::PartyMemberNames() const
{
	TArray<FString> Out;
	if (Current.IsSet()) for (const FMusubiUserPresence& P : Current->presences) Out.Add(P.username);
	return Out;
}
