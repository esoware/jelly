import { useQuery } from "@tanstack/react-query"
import type { UseMutationResult } from "@tanstack/react-query"
import { useState } from "react"

import type { AddressRecord, ChainId, KeySource, ScriptKind } from "@/ipc/bindings.gen.ts"
import { hasCode } from "@/ipc/client.ts"
import type { IpcError } from "@/ipc/client.ts"
import {
  SCRIPT_LABELS,
  chainMeta,
  chainName,
  examplePath,
  hasScriptChoice,
  keyHolders,
} from "@/lib/chain.ts"
import { describeFailure } from "@/lib/failure.ts"
import { trimmedOrNull } from "@/lib/format.ts"
import { holdsFunds } from "@/lib/portfolio.ts"
import { addressCommands, addressKeys } from "@/query/address.ts"
import { moneyKeys, moneyQueries } from "@/query/money.ts"
import { useIpcMutation } from "@/query/mutation.ts"
import { txKeys } from "@/query/tx.ts"
import { Button } from "@/ui/button.tsx"
import { Callout } from "@/ui/callout.tsx"
import {
  Dialog,
  DialogBody,
  DialogClose,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogPopup,
  DialogTitle,
} from "@/ui/dialog.tsx"
import type { DialogStateProps } from "@/ui/dialog.tsx"
import { Field } from "@/ui/field.tsx"
import { Identifier } from "@/ui/identifier.tsx"
import { Input } from "@/ui/input.tsx"
import { PasswordInput } from "@/ui/password-input.tsx"
import { Select, SelectPopup, SelectItem, SelectTrigger } from "@/ui/select.tsx"
import { Textarea } from "@/ui/textarea.tsx"
import { DetailList, IdentifierWithCopy } from "@/widgets/copyable.tsx"
import type { DetailRow } from "@/widgets/copyable.tsx"
import { FormDialog, TypedConfirmDialog } from "@/widgets/dialogs.tsx"

interface AddAddressOptions {
  mutationFn: () => Promise<null>
  errorTitle: string
  successToast: string
  onDone: () => void
}

function useAddAddress({
  mutationFn,
  errorTitle,
  successToast,
  onDone,
}: AddAddressOptions): UseMutationResult<null, IpcError, void> {
  return useIpcMutation<null>({
    mutationFn,
    errorTitle,
    invalidates: [addressKeys.root, moneyKeys.root, txKeys.root],
    successToast,
    onDone,
  })
}

type ScriptChoice = ScriptKind | "default"

const DEFAULT_CHOICE = "default"
const DEFAULT_LABEL = "Chain default"

function defaultLabel(chain: ChainId): string {
  const kind = chainMeta(chain).defaultScript
  return kind === null ? DEFAULT_LABEL : `${DEFAULT_LABEL}, ${SCRIPT_LABELS[kind]}`
}

interface ScriptKindSelectProps {
  chain: ChainId
  value: ScriptKind | null
  onValueChange: (value: ScriptKind | null) => void
}

function ScriptKindSelect({ chain, value, onValueChange }: ScriptKindSelectProps) {
  if (!hasScriptChoice(chain)) {
    return null
  }
  const kinds = chainMeta(chain).scriptKinds
  const chainDefault = defaultLabel(chain)

  return (
    <Field label="Address type">
      <Select<ScriptChoice>
        value={value ?? DEFAULT_CHOICE}
        onValueChange={(next) => {
          onValueChange(next === DEFAULT_CHOICE ? null : next)
        }}
      >
        <SelectTrigger className="w-full">
          {value === null ? chainDefault : SCRIPT_LABELS[value]}
        </SelectTrigger>
        <SelectPopup>
          <SelectItem value={DEFAULT_CHOICE}>{chainDefault}</SelectItem>
          {kinds.map((kind) => (
            <SelectItem key={kind} value={kind}>
              {SCRIPT_LABELS[kind]}
            </SelectItem>
          ))}
        </SelectPopup>
      </Select>
    </Field>
  )
}

interface OptionalLabelFieldProps {
  value: string
  onValueChange: (value: string) => void
}

function OptionalLabelField({ value, onValueChange }: OptionalLabelFieldProps) {
  return (
    <Field label="Optional label">
      <Input
        value={value}
        autoComplete="off"
        onChange={(e) => {
          onValueChange(e.target.value)
        }}
      />
    </Field>
  )
}

const DERIVED_DESCRIPTION =
  "Derive a new address from your seed phrase, at the lowest unused index."

interface NewAddressDialogProps extends DialogStateProps {
  chain: ChainId
}

export function NewAddressDialog({
  open,
  onOpenChange,
  onOpenChangeComplete,
  chain,
}: NewAddressDialogProps) {
  const [label, setLabel] = useState("")
  const [script, setScript] = useState<ScriptKind | null>(null)
  const isSubaddress = chainMeta(chain).hasSubaddresses

  const add = useAddAddress({
    mutationFn: () => addressCommands.add(chain, script, trimmedOrNull(label)),
    errorTitle: "Could not add the address",
    successToast: "Address added",
    onDone: () => {
      onOpenChange(false)
    },
  })

  return (
    <FormDialog
      open={open}
      onOpenChange={onOpenChange}
      onOpenChangeComplete={onOpenChangeComplete}
      size="sm"
      title={isSubaddress ? "New subaddress" : "New address"}
      description={
        isSubaddress
          ? `Derive a new subaddress of your ${chainName(chain)} account, at the lowest unused index.`
          : DERIVED_DESCRIPTION
      }
      submitLabel="Add"
      isBusy={add.isPending}
      error={add.error === null ? null : describeFailure(add.error)}
      onSubmit={() => {
        add.mutate()
      }}
    >
      <ScriptKindSelect chain={chain} value={script} onValueChange={setScript} />
      <OptionalLabelField value={label} onValueChange={setLabel} />
    </FormDialog>
  )
}

interface CustomPathDialogProps extends DialogStateProps {
  chain: ChainId
}

export function CustomPathDialog({
  open,
  onOpenChange,
  onOpenChangeComplete,
  chain,
}: CustomPathDialogProps) {
  const [path, setPath] = useState("")
  const [label, setLabel] = useState("")
  const [script, setScript] = useState<ScriptKind | null>(null)
  const trimmedPath = path.trim()

  const add = useAddAddress({
    mutationFn: () => addressCommands.addAt(chain, trimmedPath, script, trimmedOrNull(label)),
    errorTitle: "Could not add the address",
    successToast: "Address added",
    onDone: () => {
      onOpenChange(false)
    },
  })

  const isRejected = hasCode(add.error, "invalid_input")
  const message = add.error === null ? null : describeFailure(add.error)
  const example = examplePath(chain)

  return (
    <FormDialog
      open={open}
      onOpenChange={onOpenChange}
      onOpenChangeComplete={onOpenChangeComplete}
      size="sm"
      title="Address at a custom path"
      description={
        hasScriptChoice(chain)
          ? "The path determines the address type unless you choose one below."
          : undefined
      }
      submitLabel="Add"
      isSubmitDisabled={trimmedPath === ""}
      isBusy={add.isPending}
      error={isRejected ? null : message}
      onSubmit={() => {
        add.mutate()
      }}
    >
      <Field
        label="Derivation path"
        error={isRejected ? message : null}
        hint={`The default path is ${example}.`}
      >
        <Input
          isMono
          value={path}
          autoComplete="off"
          placeholder={example}
          onChange={(e) => {
            setPath(e.target.value)
          }}
        />
      </Field>

      <ScriptKindSelect chain={chain} value={script} onValueChange={setScript} />

      <OptionalLabelField value={label} onValueChange={setLabel} />
    </FormDialog>
  )
}

const GENERATE_KEY_DESCRIPTION =
  "The key is stored only in this wallet's vault, and your seed phrase cannot restore it. Export it to back it up."

interface GenerateKeyDialogProps extends DialogStateProps {
  chain: ChainId
}

export function GenerateKeyDialog({
  open,
  onOpenChange,
  onOpenChangeComplete,
  chain,
}: GenerateKeyDialogProps) {
  const [label, setLabel] = useState("")
  const [script, setScript] = useState<ScriptKind | null>(null)

  const generate = useAddAddress({
    mutationFn: () => addressCommands.generate(chain, script, trimmedOrNull(label)),
    errorTitle: "Could not generate the key",
    successToast: "Key generated",
    onDone: () => {
      onOpenChange(false)
    },
  })

  return (
    <FormDialog
      open={open}
      onOpenChange={onOpenChange}
      onOpenChangeComplete={onOpenChangeComplete}
      size="sm"
      title="Generate a standalone key"
      description={GENERATE_KEY_DESCRIPTION}
      submitLabel="Generate"
      isBusy={generate.isPending}
      error={generate.error === null ? null : describeFailure(generate.error)}
      onSubmit={() => {
        generate.mutate()
      }}
    >
      <ScriptKindSelect chain={chain} value={script} onValueChange={setScript} />
      <OptionalLabelField value={label} onValueChange={setLabel} />
    </FormDialog>
  )
}

const IMPORT_KEY_DESCRIPTION =
  "Paste a WIF or Electrum export, raw hex, a Solana keypair, or an EVM keystore v3 file."

const TON_DESCRIPTION =
  "Import a 32-byte Ed25519 seed or a native 24-word TON phrase as a V5R1 wallet. For a BIP-39 seed phrase, restore a wallet instead."

const KEY_ROWS = 4

const KEYSTORE_MARKER = /"crypto"|"Crypto"|"ciphertext"/u

function isKeystore(text: string): boolean {
  return (text.startsWith("{") || text.startsWith("[")) && KEYSTORE_MARKER.test(text)
}

interface ImportKeyDialogProps extends DialogStateProps {
  chain: ChainId
}

export function ImportKeyDialog({
  open,
  onOpenChange,
  onOpenChangeComplete,
  chain,
}: ImportKeyDialogProps) {
  const [material, setMaterial] = useState("")
  const [keyPassword, setKeyPassword] = useState("")
  const [label, setLabel] = useState("")
  const [script, setScript] = useState<ScriptKind | null>(null)

  const trimmedMaterial = material.trim()
  const isTonPhrase = chain === "ton" && trimmedMaterial.split(/\s+/u).length > 1
  const hasPasswordField = isKeystore(trimmedMaterial) || isTonPhrase

  const importKey = useAddAddress({
    mutationFn: () =>
      addressCommands.importPrivateKey(
        chain,
        trimmedMaterial,
        hasPasswordField && keyPassword !== "" ? keyPassword : null,
        script,
        trimmedOrNull(label),
      ),
    errorTitle: "Could not import the key",
    successToast: "Key imported",
    onDone: () => {
      onOpenChange(false)
    },
  })

  return (
    <FormDialog
      open={open}
      onOpenChange={onOpenChange}
      onOpenChangeComplete={onOpenChangeComplete}
      title="Import a private key"
      description={chain === "ton" ? TON_DESCRIPTION : IMPORT_KEY_DESCRIPTION}
      submitLabel="Import"
      isSubmitDisabled={trimmedMaterial === ""}
      isBusy={importKey.isPending}
      error={importKey.error === null ? null : describeFailure(importKey.error)}
      onSubmit={() => {
        importKey.mutate()
      }}
    >
      <Field label="Private key">
        <Textarea
          rows={KEY_ROWS}
          autoComplete="off"
          value={material}
          onChange={(e) => {
            setMaterial(e.target.value)
          }}
        />
      </Field>

      {hasPasswordField ? (
        <Field
          label={isTonPhrase ? "Seed password" : "Key file password"}
          hint={
            isTonPhrase
              ? "Leave blank unless your TON phrase was created with a password."
              : "Enter the password used to encrypt this key file."
          }
        >
          <PasswordInput
            autoComplete="off"
            value={keyPassword}
            onChange={(e) => {
              setKeyPassword(e.target.value)
            }}
          />
        </Field>
      ) : null}

      <ScriptKindSelect chain={chain} value={script} onValueChange={setScript} />

      <OptionalLabelField value={label} onValueChange={setLabel} />
    </FormDialog>
  )
}

const PATH_NOTE =
  "Its balance and history leave this wallet, but your seed phrase still holds the key. New address hands out the lowest unused index first, and Address at a custom path recreates any path."

const STANDALONE_NOTE =
  "Its balance and history leave this wallet, but the standalone key stays with its other addresses."

const SUBADDRESS_NOTE =
  "Its balance and history leave this wallet, but your seed phrase still holds the key. New subaddress hands out the lowest unused index first, so it brings this one back."

function locationOf(source: KeySource): DetailRow {
  switch (source.kind) {
    case "derived": {
      return { label: "Path", value: source.path, copy: source.path }
    }
    case "subaddress": {
      return { label: "Subaddress", value: `${String(source.major)}/${String(source.minor)}` }
    }
    case "imported": {
      return { label: "Key", value: "Standalone key" }
    }
  }
}

function noteFor(source: KeySource): string {
  switch (source.kind) {
    case "derived": {
      return PATH_NOTE
    }
    case "subaddress": {
      return SUBADDRESS_NOTE
    }
    case "imported": {
      return STANDALONE_NOTE
    }
  }
}

interface RemoveAddressDialogProps extends DialogStateProps {
  row: AddressRecord
}

export function RemoveAddressDialog({
  open,
  onOpenChange,
  onOpenChangeComplete,
  row,
}: RemoveAddressDialogProps) {
  const balances = useQuery(moneyQueries.balances())
  const hasFunds = holdsFunds(balances.data ?? [], [row.id])

  const remove = useIpcMutation<null, number>({
    mutationFn: addressCommands.remove,
    errorTitle: "Could not remove the address",
    invalidates: [addressKeys.root, moneyKeys.root, txKeys.root],
    successToast: "Address removed",
    onDone: () => {
      onOpenChange(false)
    },
  })

  return (
    <Dialog
      open={open}
      isLocked={remove.isPending}
      onOpenChange={onOpenChange}
      onOpenChangeComplete={onOpenChangeComplete}
    >
      <DialogPopup size="sm" hasCloseButton={!remove.isPending}>
        <DialogHeader>
          <DialogTitle>Remove address</DialogTitle>
          <DialogDescription render={<div />}>
            <IdentifierWithCopy value={row.address} />
          </DialogDescription>
        </DialogHeader>

        <DialogBody>
          <DetailList items={[locationOf(row.source)]} />
          <p className="text-caption text-fg-muted">{noteFor(row.source)}</p>
          {hasFunds ? (
            <Callout tone="warn" heading="This address still holds funds">
              Jelly stops tracking them until you add the address back.
            </Callout>
          ) : null}
        </DialogBody>

        <DialogFooter>
          <DialogClose render={<Button variant="secondary" disabled={remove.isPending} />}>
            Cancel
          </DialogClose>
          <Button
            variant="danger"
            isLoading={remove.isPending}
            onClick={() => {
              remove.mutate(row.id)
            }}
          >
            Remove
          </Button>
        </DialogFooter>
      </DialogPopup>
    </Dialog>
  )
}

const DELETE_PHRASE = "DELETE"

interface DeleteKey {
  addressId: number
  password: string
  confirmation: string
}

interface DeleteKeyDialogProps extends DialogStateProps {
  rows: readonly AddressRecord[]
  addressId: number
}

export function DeleteKeyDialog({
  open,
  onOpenChange,
  onOpenChangeComplete,
  rows,
  addressId,
}: DeleteKeyDialogProps) {
  const balances = useQuery(moneyQueries.balances())
  const deleteKey = useIpcMutation<null, DeleteKey>({
    mutationFn: (variables) =>
      addressCommands.deleteKey(variables.addressId, variables.password, variables.confirmation),
    invalidates: [addressKeys.root, moneyKeys.root, txKeys.root],
    shouldRefreshOnError: true,
    errorTitle: "Could not delete the key",
    successToast: "Key deleted",
    onDone: () => {
      onOpenChange(false)
    },
  })

  const row = rows.find((entry) => entry.id === addressId)
  const holders = row === undefined ? [] : keyHolders(rows, row)
  const others = holders.filter((entry) => entry.id !== addressId)
  const hasFunds = holdsFunds(
    balances.data ?? [],
    holders.map((holder) => holder.id),
  )

  return (
    <TypedConfirmDialog
      open={open}
      onOpenChange={onOpenChange}
      onOpenChangeComplete={onOpenChangeComplete}
      title="Delete key"
      description={
        <>
          Delete the private key for{" "}
          <Identifier
            value={row?.address ?? ""}
            className="font-mono text-body break-all select-text"
          />
          . The seed phrase cannot restore it, so export it first unless you no longer need it.
          {hasFunds ? " Funds are still held under this key." : null}
          {others.length > 0 ? (
            <>
              {" Its other addresses go with it."}
              <span className="mt-1 flex flex-col gap-1">
                {others.map((entry) => (
                  <Identifier
                    key={entry.id}
                    value={entry.address}
                    isTruncated
                    className="font-mono select-text"
                  />
                ))}
              </span>
            </>
          ) : null}
        </>
      }
      confirmationText={DELETE_PHRASE}
      confirmLabel="Delete key"
      isBusy={deleteKey.isPending}
      error={deleteKey.error}
      onConfirm={(confirmation, password) => {
        deleteKey.mutate({ addressId, password, confirmation })
      }}
    />
  )
}
