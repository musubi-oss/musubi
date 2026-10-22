#include "MusubiAuthorityComponent.h"
#include "MusubiSimulatorSubsystem.h"
#include "MusubiJson.h"
#include "Components/PrimitiveComponent.h"
#include "GameFramework/Actor.h"

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

void UMusubiAuthorityComponent::CapturePhysicsIntoComponents()
{
	UPrimitiveComponent* B = Body();
	if (!B || !B->IsSimulatingPhysics()) return;
	FMusubiJson V; V.Value = VecJson(B->GetPhysicsLinearVelocity());
	FMusubiJson W; W.Value = VecJson(B->GetPhysicsAngularVelocityInRadians());
	Components.FindOrAdd(VelocityComponent()).Value = V;
	Components.FindOrAdd(AngularVelocityComponent()).Value = W;
	Dirty.Add(VelocityComponent());
	Dirty.Add(AngularVelocityComponent());
}

void UMusubiAuthorityComponent::RestorePhysicsFromComponents()
{
	UPrimitiveComponent* B = Body();
	if (!B) return;
	FVector V;
	if (const FState* S = Components.Find(VelocityComponent()); S && VecFromJson(S->Value, V)) B->SetPhysicsLinearVelocity(V);
	if (const FState* S = Components.Find(AngularVelocityComponent()); S && VecFromJson(S->Value, V)) B->SetPhysicsAngularVelocityInRadians(V);
}

void UMusubiAuthorityComponent::SetPhysicsEnabled(bool bEnable)
{
	UPrimitiveComponent* B = Body();
	if (!B) return;
	// ghosts and frozen bodies stay kinematic: Chaos sees them for contacts, nothing moves them but the node
	B->SetSimulatePhysics(bEnable);
}
