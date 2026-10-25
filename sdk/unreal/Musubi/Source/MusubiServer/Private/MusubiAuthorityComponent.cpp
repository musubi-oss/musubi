#include "MusubiAuthorityComponent.h"
#include "MusubiServerModule.h"
#include "MusubiEntityRegistry.h"
#include "MusubiSimulatorSubsystem.h"
#include "MusubiJson.h"
#include "Components/PrimitiveComponent.h"
#include "GameFramework/Actor.h"
#include "Net/UnrealNetwork.h"
#include "Engine/NetDriver.h"
#include "Engine/World.h"
#include "MusubiSimulatorSettings.h"
#include "Runtime/Launch/Resources/Version.h"
#include "Components/SkeletalMeshComponent.h"
#include "PhysicsEngine/BodyInstance.h"
#include "PhysicsEngine/BodySetup.h"
#include "PhysicsEngine/ConstraintInstance.h"
#include "PhysicsEngine/PhysicsConstraintComponent.h"

UMusubiAuthorityComponent::FMusubiOnClientEntityIdentified UMusubiAuthorityComponent::OnClientEntityIdentified;
UMusubiAuthorityComponent::FMusubiOnServerEntityBound UMusubiAuthorityComponent::OnServerEntityBound;

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
	OnServerEntityBound.Broadcast(this);
}

bool UMusubiAuthorityComponent::GhostRoleDowngradeEnabled(const AActor* A)
{
	const UMusubiSimulatorSettings* S = GetDefault<UMusubiSimulatorSettings>();
	if (!S || !S->bGhostsAsSimulatedProxy || !A) return false;
	// before 5.8 the engine Iris bridge accepts only ROLE_Authority actors — but it checks once, in StartReplicatingActor
	// (spawn / SetReplicates), so an actor that enters replication as authority and is downgraded afterwards keeps
	// replicating (the simulator calls PreReplication for it, PreReplicateGhosts)
	return true;
}

static bool IsIrisDriver(const AActor* A)
{
#if UE_WITH_IRIS
	const UWorld* W = A ? A->GetWorld() : nullptr;
	const UNetDriver* D = W ? W->GetNetDriver() : nullptr;
	return D && D->GetReplicationSystem();
#else
	return false;
#endif
}

// RemoteRole has no public setter (SetRemoteRoleForBackwardsCompat is protected, ExchangeNetRoles is one-way), so write the
// property the way UNetDriver itself finds it.
static void SetRemoteRoleOf(AActor* A, ENetRole R)
{
	static FProperty* P = FindFProperty<FProperty>(AActor::StaticClass(), TEXT("RemoteRole"));
	if (P) *P->ContainerPtrToValuePtr<uint8>(A) = (uint8)R;
}

void UMusubiAuthorityComponent::ApplyNetRole(bool bIsOwned)
{
	AActor* A = GetOwner();
	if (!A || !A->GetIsReplicated() || !GhostRoleDowngradeEnabled(A)) return;
	// ghost: Role=SimulatedProxy — HasAuthority() is false for every Blueprint and plugin. What this server's clients get as
	// their Role is the wire RemoteRole: classic swaps the pair for the duration of ServerReplicateActors
	// (SetRoleSwapOnReplicate, so the ghost's RemoteRole stays Authority as on any actor); Iris sends RemoteRole as is
	// (FNetRoleNetSerializer dequantizes it into the client's Role, no swap) so there the ghost's RemoteRole must be
	// SimulatedProxy — Authority would make the client's copy HasAuthority()
	const bool bIris = IsIrisDriver(A);
	A->SetRole(bIsOwned ? ROLE_Authority : ROLE_SimulatedProxy);
	SetRemoteRoleOf(A, (bIsOwned || bIris) ? ROLE_SimulatedProxy : ROLE_Authority);
	if (UNetDriver* D = A->GetNetDriver()) D->SetRoleSwapOnReplicate(A, !bIsOwned && !bIris);
	A->ForceNetUpdate();
}

void UMusubiAuthorityComponent::SetOwned(bool bIsOwned)
{
	if (bOwned == bIsOwned) return;
	bOwned = bIsOwned;
	bNetOwned = bIsOwned;
	ApplyNetRole(bIsOwned);
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


// Multi-body actors (a ragdoll: one FBodyInstance per bone of a USkeletalMeshComponent; a compound prop: several simulating
// primitives): every simulating body beyond the root goes over as [name, p(3), q(4), v(3), w(3), awake], pose relative to
// the root body so the node's position of the entity stays the anchor; quantised to 0.01 cm / 1e-4 / 0.1 cm/s.
static TSharedPtr<FJsonValue> Num(double D, double Step) { return MakeShared<FJsonValueNumber>(FMath::RoundToDouble(D / Step) * Step); }

static void ForEachBody(AActor* A, UPrimitiveComponent* Root, TFunctionRef<void(const FString&, FBodyInstance*, USkeletalMeshComponent*)> Fn)
{
	for (UActorComponent* C : A->GetComponents())
	{
		if (USkeletalMeshComponent* SK = Cast<USkeletalMeshComponent>(C))
		{
			for (FBodyInstance* BI : SK->Bodies)
				if (BI && BI->IsValidBodyInstance() && BI->BodySetup.IsValid()) Fn(SK->GetName() + TEXT(":") + BI->BodySetup->BoneName.ToString(), BI, SK);
		}
		else if (UPrimitiveComponent* PC = Cast<UPrimitiveComponent>(C); PC && PC != Root)
			if (FBodyInstance* BI = PC->GetBodyInstance(); BI && BI->IsValidBodyInstance()) Fn(PC->GetName(), BI, nullptr);
	}
}

static void CaptureBodies(AActor* A, UPrimitiveComponent* Root, const TSharedPtr<FJsonObject>& P, TArray<TSharedPtr<FJsonValue>>& Broken)
{
	const FTransform RootXf = Root->GetComponentTransform();
	TArray<TSharedPtr<FJsonValue>> Bodies;
	ForEachBody(A, Root, [&](const FString& Name, FBodyInstance* BI, USkeletalMeshComponent*)
	{
		if (!BI->IsInstanceSimulatingPhysics()) return;
		const FTransform Rel = BI->GetUnrealWorldTransform().GetRelativeTransform(RootXf);
		const FVector L = Rel.GetLocation(), V = BI->GetUnrealWorldVelocity(), W = BI->GetUnrealWorldAngularVelocityInRadians();
		const FQuat Q = Rel.GetRotation();
		TArray<TSharedPtr<FJsonValue>> R;
		R.Add(MakeShared<FJsonValueString>(Name));
		for (double D : { L.X, L.Y, L.Z }) R.Add(Num(D, 0.01));
		for (double D : { Q.X, Q.Y, Q.Z, Q.W }) R.Add(Num(D, 0.0001));
		for (double D : { V.X, V.Y, V.Z, W.X, W.Y, W.Z }) R.Add(Num(D, 0.1));
		R.Add(MakeShared<FJsonValueBoolean>(BI->IsInstanceAwake()));
		Bodies.Add(MakeShared<FJsonValueArray>(R));
	});
	if (Bodies.Num()) P->SetArrayField(TEXT("bodies"), Bodies);
	for (UActorComponent* C : A->GetComponents())
		if (USkeletalMeshComponent* SK = Cast<USkeletalMeshComponent>(C))
			for (FConstraintInstance* CI : SK->Constraints)
				if (CI && CI->IsTerminated()) Broken.Add(MakeShared<FJsonValueString>(SK->GetName() + TEXT(":") + CI->JointName.ToString()));
}

static void RestoreBodies(AActor* A, UPrimitiveComponent* Root, const TArray<TSharedPtr<FJsonValue>>& Bodies)
{
	const FTransform RootXf = Root->GetComponentTransform();
	TMap<FString, FBodyInstance*> ByName;
	TSet<USkeletalMeshComponent*> Meshes;
	ForEachBody(A, Root, [&](const FString& Name, FBodyInstance* BI, USkeletalMeshComponent* SK) { ByName.Add(Name, BI); if (SK) Meshes.Add(SK); });
	// a skeletal mesh switches all its bodies on at once (a ragdoll that was simulating on the old owner simulates here)
	for (USkeletalMeshComponent* SK : Meshes)
		for (const TSharedPtr<FJsonValue>& B : Bodies)
		{
			const TArray<TSharedPtr<FJsonValue>>* R = nullptr;
			if (B.IsValid() && B->TryGetArray(R) && R && R->Num() >= 15 && (*R)[0]->AsString().StartsWith(SK->GetName() + TEXT(":"))) { if (!SK->IsSimulatingPhysics()) SK->SetAllBodiesSimulatePhysics(true); break; }
		}
	int32 Applied = 0;
	double MaxErr = 0;
	for (const TSharedPtr<FJsonValue>& B : Bodies)
	{
		const TArray<TSharedPtr<FJsonValue>>* R = nullptr;
		if (!B.IsValid() || !B->TryGetArray(R) || !R || R->Num() < 15) continue;
		FBodyInstance** Found = ByName.Find((*R)[0]->AsString());
		if (!Found || !*Found) continue;
		FBodyInstance* BI = *Found;
		auto N = [&](int32 i) { return (*R)[i]->AsNumber(); };
		const FTransform Rel(FQuat(N(4), N(5), N(6), N(7)).GetNormalized(), FVector(N(1), N(2), N(3)));
		const FVector V(N(8), N(9), N(10)), W(N(11), N(12), N(13));
		const FTransform Target = Rel * RootXf;
		if (!BI->IsInstanceSimulatingPhysics()) BI->SetInstanceSimulatePhysics(true);
		BI->SetBodyTransform(Target, ETeleportType::TeleportPhysics);
		BI->SetLinearVelocity(V, false);
		BI->SetAngularVelocityInRadians(W, false);
		if ((*R)[14]->AsBool()) BI->WakeInstance(); else BI->PutInstanceToSleep();
		MaxErr = FMath::Max(MaxErr, FVector::Dist(BI->GetUnrealWorldTransform().GetLocation(), Target.GetLocation()));
		++Applied;
	}
	UE_LOG(LogMusubiServer, Log, TEXT("%s: bodies restored %d of %d, max pose err %.2f cm"), *A->GetName(), Applied, Bodies.Num(), MaxErr);
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
	if (AActor* A = GetOwner()) CaptureBodies(A, B, P, Broken);
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
	const TArray<TSharedPtr<FJsonValue>>* Bodies = nullptr;
	if ((*P)->TryGetArrayField(TEXT("bodies"), Bodies) && Bodies && GetOwner()) RestoreBodies(GetOwner(), B, *Bodies);
	const TArray<TSharedPtr<FJsonValue>>* Broken = nullptr;
	if ((*P)->TryGetArrayField(TEXT("broken"), Broken) && Broken)
		if (AActor* A = GetOwner())
			for (UActorComponent* C : A->GetComponents())
				if (UPhysicsConstraintComponent* K = Cast<UPhysicsConstraintComponent>(C))
					if (!K->IsBroken())
						for (const TSharedPtr<FJsonValue>& N : *Broken)
							if (N.IsValid() && N->AsString() == K->GetName()) { K->BreakConstraint(); break; }
	if ((*P)->TryGetArrayField(TEXT("broken"), Broken) && Broken)
		if (AActor* A = GetOwner())
			for (UActorComponent* C : A->GetComponents())
				if (USkeletalMeshComponent* SK = Cast<USkeletalMeshComponent>(C))
					for (const TSharedPtr<FJsonValue>& N : *Broken)
					{
						FString Mesh, Joint;
						if (N.IsValid() && N->AsString().Split(TEXT(":"), &Mesh, &Joint) && Mesh == SK->GetName()) SK->BreakConstraint(FVector::ZeroVector, SK->GetComponentLocation(), *Joint);
					}
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
