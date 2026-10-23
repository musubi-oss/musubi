#include "MusubiAuthorityComponent.h"
#include "MusubiEntityRegistry.h"
#include "MusubiSimulatorSubsystem.h"
#include "MusubiJson.h"
#include "Components/PrimitiveComponent.h"
#include "GameFramework/Actor.h"
#include "Net/UnrealNetwork.h"
#include "PhysicsEngine/PhysicsConstraintComponent.h"

UMusubiAuthorityComponent::FMusubiOnClientEntityIdentified UMusubiAuthorityComponent::OnClientEntityIdentified;

void UMusubiAuthorityComponent::GetLifetimeReplicatedProps(TArray<FLifetimeProperty>& OutLifetimeProps) const
{
	Super::GetLifetimeReplicatedProps(OutLifetimeProps);
	DOREPLIFETIME(UMusubiAuthorityComponent, NetEntityId);
	DOREPLIFETIME(UMusubiAuthorityComponent, bNetOwned);
}

void UMusubiAuthorityComponent::OnRep_NetEntityId()
{
	if (NetEntityId.IsEmpty()) return;
	// registry first: its OnEntityActorChanged fires while the old twin is still alive, the driver destroys it right after
	if (UMusubiEntityRegistry* R = UMusubiEntityRegistry::Get(GetOwner())) R->Register(NetEntityId, GetOwner());
	OnClientEntityIdentified.Broadcast(GetOwner(), NetEntityId);
}

void UMusubiAuthorityComponent::EndPlay(const EEndPlayReason::Type Reason)
{
	const FString Id = NetEntityId.IsEmpty() ? EntityId.ToString() : NetEntityId;
	if (UMusubiEntityRegistry* R = Id.IsEmpty() ? nullptr : UMusubiEntityRegistry::Get(GetOwner())) R->Unregister(Id, GetOwner());
	Super::EndPlay(Reason);
}

FString UMusubiAuthorityComponent::GetComponentJson(const FString& Name) const
{
	const FState* S = Components.Find(Name);
	return S ? S->Value.ToText() : FString();
}

double UMusubiAuthorityComponent::GetComponentNumber(const FString& Name, double Default) const
{
	const FState* S = Components.Find(Name);
	double V;
	return S && S->Value.Value.IsValid() && S->Value.Value->TryGetNumber(V) ? V : Default;
}

FString UMusubiAuthorityComponent::GetComponentString(const FString& Name, const FString& Default) const
{
	const FState* S = Components.Find(Name);
	FString V;
	return S && S->Value.Value.IsValid() && S->Value.Value->TryGetString(V) ? V : Default;
}

const FMusubiJson* UMusubiAuthorityComponent::FindComponent(const FString& Name) const
{
	const FState* S = Components.Find(Name);
	return S ? &S->Value : nullptr;
}

TArray<FString> UMusubiAuthorityComponent::ComponentNames() const
{
	TArray<FString> Out;
	Components.GetKeys(Out);
	return Out;
}

void UMusubiAuthorityComponent::SetComponent(const FString& Name, const FMusubiJson& Value)
{
	if (!bOwned) return;
	FState& S = Components.FindOrAdd(Name);
	S.Value = Value;
	Dirty.Add(Name);
	OnComponentChanged.Broadcast(Name, Value.ToText());
}

void UMusubiAuthorityComponent::SetComponentJson(const FString& Name, const FString& ValueJson) { SetComponent(Name, FMusubiJson::FromText(ValueJson)); }
void UMusubiAuthorityComponent::SetComponentNumber(const FString& Name, double Value)
{
	FMusubiJson J; J.Value = MakeShared<FJsonValueNumber>(Value); SetComponent(Name, J);
}
void UMusubiAuthorityComponent::SetComponentString(const FString& Name, const FString& Value)
{
	FMusubiJson J; J.Value = MakeShared<FJsonValueString>(Value); SetComponent(Name, J);
}

void UMusubiAuthorityComponent::RequestDespawn()
{
	if (bOwned && Sim.IsValid()) Sim->Despawn(EntityId);
}

void UMusubiAuthorityComponent::Interact(const FString& Kind, const FString& PayloadJson, const FString& TargetEntityId)
{
	if (!bOwned || !Sim.IsValid()) return;
	TOptional<FMusubiUuid> Target;
	FMusubiUuid T;
	if (!TargetEntityId.IsEmpty() && FMusubiUuid::Parse(TargetEntityId, T)) Target = T;
	// the command's `entity` is the one being acted on (a ghost → routed to its owner); we are `from`
	Sim->Interact(Target.IsSet() ? Target.GetValue() : EntityId, Kind, FMusubiJson::FromText(PayloadJson), EntityId);
}

void UMusubiAuthorityComponent::Bind(UMusubiSimulatorSubsystem* Owner, const FMusubiUuid& Id, const FMusubiCellId& InCell, bool bIsOwned)
{
	Sim = Owner;
	EntityId = Id;
	NetEntityId = Id.ToString();
	if (UMusubiEntityRegistry* R = UMusubiEntityRegistry::Get(GetOwner())) R->Register(NetEntityId, GetOwner());
	Cell = InCell;
	Components.Reset();
	Dirty.Reset();
	bInHandover = false;
	bOwned = !bIsOwned;
	SetOwned(bIsOwned);
}

void UMusubiAuthorityComponent::SetOwned(bool bIsOwned)
{
	if (bOwned == bIsOwned) return;
	bOwned = bIsOwned;
	bNetOwned = bIsOwned;
	SetPhysicsEnabled(bIsOwned);
	OnAuthorityChanged.Broadcast(bIsOwned);
}

bool UMusubiAuthorityComponent::ApplyFromNode(const FMusubiWorldEntity& E)
{
	bool bChanged = false;
	for (const FMusubiWorldComponent& C : E.components)
	{
		FState* S = Components.Find(C.name);
		if (S && C.version != 0 && C.version <= S->Version) continue;
		FState& T = Components.FindOrAdd(C.name);
		T.Value = C.value;
		T.Version = C.version;
		bChanged = true;
		OnComponentChanged.Broadcast(C.name, C.value.ToText());
		OnComponentApplied.Broadcast(C.name, T.Value);
	}
	return bChanged;
}

TSharedPtr<FJsonObject> UMusubiAuthorityComponent::TakeDirty()
{
	if (Dirty.Num() == 0) return nullptr;
	TSharedPtr<FJsonObject> O = MakeShared<FJsonObject>();
	for (const FString& N : Dirty)
		if (const FState* S = Components.Find(N)) O->SetField(N, MusubiJson::JsonOrNull(S->Value));
	Dirty.Reset();
	return O;
}

UPrimitiveComponent* UMusubiAuthorityComponent::Body() const
{
	AActor* A = GetOwner();
	return A ? Cast<UPrimitiveComponent>(A->GetRootComponent()) : nullptr;
}

static TSharedPtr<FJsonValue> VecJson(const FVector& V)
{
	TArray<TSharedPtr<FJsonValue>> A;
	A.Add(MakeShared<FJsonValueNumber>(V.X));
	A.Add(MakeShared<FJsonValueNumber>(V.Y));
	A.Add(MakeShared<FJsonValueNumber>(V.Z));
	return MakeShared<FJsonValueArray>(A);
}

static bool VecFromJson(const FMusubiJson& J, FVector& Out)
{
	const TArray<TSharedPtr<FJsonValue>>* A;
	if (!J.Value.IsValid() || !J.Value->TryGetArray(A) || A->Num() != 3) return false;
	double X, Y, Z;
	if (!(*A)[0]->TryGetNumber(X) || !(*A)[1]->TryGetNumber(Y) || !(*A)[2]->TryGetNumber(Z)) return false;
	Out = FVector(X, Y, Z);
	return true;
}

static TSharedPtr<FJsonValue> QuatJson(const FQuat& Q)
{
	TArray<TSharedPtr<FJsonValue>> A;
	A.Add(MakeShared<FJsonValueNumber>(Q.X)); A.Add(MakeShared<FJsonValueNumber>(Q.Y));
	A.Add(MakeShared<FJsonValueNumber>(Q.Z)); A.Add(MakeShared<FJsonValueNumber>(Q.W));
	return MakeShared<FJsonValueArray>(A);
}

static bool QuatFromJson(const TSharedPtr<FJsonValue>& J, FQuat& Out)
{
	const TArray<TSharedPtr<FJsonValue>>* A;
	if (!J.IsValid() || !J->TryGetArray(A) || A->Num() != 4) return false;
	Out = FQuat((*A)[0]->AsNumber(), (*A)[1]->AsNumber(), (*A)[2]->AsNumber(), (*A)[3]->AsNumber());
	if (!Out.IsNormalized()) Out.Normalize();
	return true;
}

void UMusubiAuthorityComponent::CapturePhysicsIntoComponents()
{
	UPrimitiveComponent* B = Body();
	if (!B) return;
	const bool bSim = B->IsSimulatingPhysics();
	const FVector Lin = bSim ? B->GetPhysicsLinearVelocity() : B->GetComponentVelocity();
	const FVector Ang = bSim ? B->GetPhysicsAngularVelocityInRadians() : FVector::ZeroVector;
	FMusubiJson V; V.Value = VecJson(Lin);
	FMusubiJson W; W.Value = VecJson(Ang);
	Components.FindOrAdd(VelocityComponent()).Value = V;
	Components.FindOrAdd(AngularVelocityComponent()).Value = W;
	Dirty.Add(VelocityComponent());
	Dirty.Add(AngularVelocityComponent());
	// the full rigid-body state: the receiver resumes the simulation exactly where this side stopped it
	TSharedPtr<FJsonObject> P = MakeShared<FJsonObject>();
	P->SetField(TEXT("q"), QuatJson(B->GetComponentQuat()));
	P->SetField(TEXT("v"), VecJson(Lin));
	P->SetField(TEXT("w"), VecJson(Ang));
	P->SetBoolField(TEXT("sim"), bSim);
	P->SetBoolField(TEXT("sleep"), bSim && !B->RigidBodyIsAwake());
	TArray<TSharedPtr<FJsonValue>> Broken;
	if (AActor* A = GetOwner())
		for (UActorComponent* C : A->GetComponents())
			if (UPhysicsConstraintComponent* K = Cast<UPhysicsConstraintComponent>(C))
				if (K->IsBroken()) Broken.Add(MakeShared<FJsonValueString>(K->GetName()));
	if (Broken.Num()) P->SetArrayField(TEXT("broken"), Broken);
	FMusubiJson Phys; Phys.Value = MakeShared<FJsonValueObject>(P);
	Components.FindOrAdd(PhysicsComponent()).Value = Phys;
	Dirty.Add(PhysicsComponent());
}

void UMusubiAuthorityComponent::RestorePhysicsFromComponents()
{
	UPrimitiveComponent* B = Body();
	if (!B) return;
	const FState* S = Components.Find(PhysicsComponent());
	const TSharedPtr<FJsonObject>* P = nullptr;
	if (!S || !S->Value.Value.IsValid() || !S->Value.Value->TryGetObject(P) || !P)
	{
		// older owner: velocities only
		B->SetSimulatePhysics(BodySimulates());
		FVector V;
		if (const FState* L = Components.Find(VelocityComponent()); L && VecFromJson(L->Value, V)) B->SetPhysicsLinearVelocity(V);
		if (const FState* A = Components.Find(AngularVelocityComponent()); A && VecFromJson(A->Value, V)) B->SetPhysicsAngularVelocityInRadians(V);
		return;
	}
	// order matters in Chaos: pose while kinematic, then simulate, then velocities (ignored on a kinematic body), then sleep
	FQuat Q;
	if (QuatFromJson((*P)->TryGetField(TEXT("q")), Q)) B->SetWorldRotation(Q, false, nullptr, ETeleportType::TeleportPhysics);
	bool bSim = BodySimulates();
	(*P)->TryGetBoolField(TEXT("sim"), bSim);
	bSim = bSim && BodySimulates();
	B->SetSimulatePhysics(bSim);
	if (bSim)
	{
		FVector V;
		FMusubiJson J;
		J.Value = (*P)->TryGetField(TEXT("v"));
		if (VecFromJson(J, V)) B->SetPhysicsLinearVelocity(V);
		J.Value = (*P)->TryGetField(TEXT("w"));
		if (VecFromJson(J, V)) B->SetPhysicsAngularVelocityInRadians(V);
		bool bSleep = false;
		(*P)->TryGetBoolField(TEXT("sleep"), bSleep);
		if (bSleep) B->PutRigidBodyToSleep(); else B->WakeRigidBody();
	}
	const TArray<TSharedPtr<FJsonValue>>* Broken = nullptr;
	if ((*P)->TryGetArrayField(TEXT("broken"), Broken) && Broken)
		if (AActor* A = GetOwner())
			for (UActorComponent* C : A->GetComponents())
				if (UPhysicsConstraintComponent* K = Cast<UPhysicsConstraintComponent>(C))
					if (!K->IsBroken())
						for (const TSharedPtr<FJsonValue>& N : *Broken)
							if (N.IsValid() && N->AsString() == K->GetName()) { K->BreakConstraint(); break; }
}

bool UMusubiAuthorityComponent::GetPhysicsRotation(FQuat& Out) const
{
	const FState* S = Components.Find(PhysicsComponent());
	const TSharedPtr<FJsonObject>* P = nullptr;
	return S && S->Value.Value.IsValid() && S->Value.Value->TryGetObject(P) && P && QuatFromJson((*P)->TryGetField(TEXT("q")), Out);
}

void UMusubiAuthorityComponent::SetPhysicsEnabled(bool bEnable)
{
	UPrimitiveComponent* B = Body();
	if (!B) return;
	// ghosts and frozen bodies stay kinematic: Chaos sees them for contacts, nothing moves them but the node
	B->SetSimulatePhysics(bEnable && BodySimulates());
}

bool UMusubiAuthorityComponent::BodySimulates()
{
	if (!bBodyKnown)
		if (UPrimitiveComponent* B = Body())
		{
			bBodySimulates = B->IsSimulatingPhysics();
			bBodyKnown = true;
		}
	return bBodySimulates;
}

void UMusubiAuthorityComponent::RegisterMusubiSubObject(UObject* SubObject)
{
	if (!SubObject || SubObjects.Contains(SubObject)) return;
	SubObjects.RemoveAll([](const TWeakObjectPtr<UObject>& O) { return !O.IsValid(); });
	SubObjects.Add(SubObject);
}

void UMusubiAuthorityComponent::UnregisterMusubiSubObject(UObject* SubObject)
{
	SubObjects.RemoveAll([SubObject](const TWeakObjectPtr<UObject>& O) { return !O.IsValid() || O.Get() == SubObject; });
}

UObject* UMusubiAuthorityComponent::FindMusubiSubObject(const FString& Name) const
{
	for (const TWeakObjectPtr<UObject>& O : SubObjects)
		if (UObject* S = O.Get()) if (S->GetName() == Name) return S;
	return nullptr;
}
