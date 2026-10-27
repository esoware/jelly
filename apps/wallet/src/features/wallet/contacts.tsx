import { useQuery, useSuspenseQuery } from "@tanstack/react-query"
import { useNavigate } from "@tanstack/react-router"
import { useDeferredValue, useState } from "react"
import type { RefObject } from "react"

import type { ChainId, ContactRecord } from "@/ipc/bindings.gen.ts"
import { hasCode } from "@/ipc/client.ts"
import { allChains, chainName } from "@/lib/chain.ts"
import { describeFailure } from "@/lib/failure.ts"
import { trimmedOrNull } from "@/lib/format.ts"
import { addressQueries } from "@/query/address.ts"
import { chainQueries } from "@/query/chain.ts"
import { contactCommands, contactKeys, contactQueries } from "@/query/contact.ts"
import { useIpcMutation } from "@/query/mutation.ts"
import { Button } from "@/ui/button.tsx"
import { useDialogPayload } from "@/ui/dialog.tsx"
import type { DialogStateProps } from "@/ui/dialog.tsx"
import { EmptyState } from "@/ui/empty-state.tsx"
import { Field } from "@/ui/field.tsx"
import { Icon } from "@/ui/icon.tsx"
import { Identifier } from "@/ui/identifier.tsx"
import { Input, InputButton } from "@/ui/input.tsx"
import { Menu, MenuGroup, MenuItem, MenuPopup, MenuTrigger } from "@/ui/menu.tsx"
import { Panel, PanelBody, PanelHeader } from "@/ui/panel.tsx"
import { Row, RowActions } from "@/ui/row.tsx"
import { Select, SelectItem, SelectPopup, SelectTrigger } from "@/ui/select.tsx"
import { TooltipText } from "@/ui/tooltip.tsx"
import { useAccentColor } from "@/ui/use-accent-color.ts"
import { CopyButton } from "@/widgets/copyable.tsx"
import { FormDialog } from "@/widgets/dialogs.tsx"
import { ListStatus } from "@/widgets/list-status.tsx"
import { CHAIN_BRANDS, ChainLogo } from "@/widgets/logos.tsx"

const MAX_INITIALS = 2

const GRAPHEMES = new Intl.Segmenter(undefined, { granularity: "grapheme" })

interface ContactAvatarProps {
  name: string
  chain: ChainId
  size?: "md" | "sm" | undefined
}

function ContactAvatar({ name, chain, size = "md" }: ContactAvatarProps) {
  const accentRef = useAccentColor<HTMLSpanElement>(CHAIN_BRANDS[chain].color)
  const initials = name
    .trim()
    .split(/\s+/u)
    .slice(0, MAX_INITIALS)
    .map((word) => [...GRAPHEMES.segment(word)].at(0)?.segment ?? "")
    .join("")
    .toUpperCase()

  return (
    <span
      ref={accentRef}
      data-size={size}
      className="flex shrink-0 items-center justify-center rounded-full bg-accent-active text-caption font-semibold text-fg select-none data-[size=md]:size-8 data-[size=sm]:size-6"
    >
      {initials}
    </span>
  )
}

interface ContactPickerProps {
  chain: ChainId
  value: string
  anchor: RefObject<HTMLElement | null>
  onPick: (address: string) => void
}

export function ContactPicker({ chain, value, anchor, onPick }: ContactPickerProps) {
  const navigate = useNavigate()
  const contacts = useQuery({
    ...contactQueries.all(),
    select: (rows) => rows.filter((row) => row.chain === chain),
  })
  const rows = contacts.data ?? []
  const typed = value.trim()
  const picked = rows.find((row) => row.address === typed) ?? null

  return (
    <Menu>
      <MenuTrigger render={<Button variant="subtle" size="xs" className="max-w-36" />}>
        <Icon name="contacts" />
        <span className="min-w-0 truncate">{picked?.name ?? "Contacts"}</span>
      </MenuTrigger>
      <MenuPopup align="end" anchor={anchor} shouldMatchAnchor>
        {rows.length === 0 ? (
          <>
            <p className="px-2 pt-1.5 pb-1 text-caption text-fg-faint">
              {`No saved ${chainName(chain)} addresses yet.`}
            </p>
            <MenuItem
              onClick={() => {
                void navigate({ to: "/wallet/contacts" })
              }}
            >
              <Icon name="contacts" />
              Open address book
            </MenuItem>
          </>
        ) : (
          <MenuGroup>
            {rows.map((row) => (
              <MenuItem
                key={row.id}
                onClick={() => {
                  onPick(row.address)
                }}
              >
                <span className="flex min-w-0 flex-1 items-center gap-2 py-1">
                  <ContactAvatar name={row.name} chain={row.chain} size="sm" />
                  <span className="flex min-w-0 flex-1 flex-col">
                    <span className="flex min-w-0 items-baseline gap-2">
                      <span className="max-w-full shrink-0 truncate">{row.name}</span>
                      {row.note === null ? null : (
                        <span className="min-w-0 truncate text-caption text-fg-faint">
                          {row.note}
                        </span>
                      )}
                    </span>
                    <Identifier
                      value={row.address}
                      isTruncated
                      hasTooltip={false}
                      className="font-mono text-caption text-fg-muted"
                    />
                  </span>
                  {row.id === picked?.id ? (
                    <Icon name="check" className="size-icon-md shrink-0 text-fg-muted" />
                  ) : null}
                </span>
              </MenuItem>
            ))}
          </MenuGroup>
        )}
      </MenuPopup>
    </Menu>
  )
}

interface ContactSearchProps {
  value: string
  onValueChange: (value: string) => void
}

function ContactSearch({ value, onValueChange }: ContactSearchProps) {
  return (
    <Input
      size="sm"
      placeholder="Search"
      autoComplete="off"
      spellCheck={false}
      className="w-44"
      value={value}
      icon={<Icon name="search" />}
      attachment={
        value === "" ? null : (
          <InputButton
            onClick={() => {
              onValueChange("")
            }}
          >
            <Icon name="close" />
          </InputButton>
        )
      }
      onChange={(e) => {
        onValueChange(e.target.value)
      }}
      onKeyDown={(e) => {
        if (e.key === "Escape" && value !== "") {
          e.preventDefault()
          onValueChange("")
        }
      }}
    />
  )
}

interface ContactTileProps {
  contact: ContactRecord
  onEdit: () => void
  onRemove: () => void
}

function ContactTile({ contact, onEdit, onRemove }: ContactTileProps) {
  return (
    <Row className="rounded-lg bg-surface" onActivate={onEdit}>
      <ContactAvatar name={contact.name} chain={contact.chain} />
      <span className="flex min-w-0 flex-1 flex-col">
        <span className="flex min-w-0 items-center gap-2">
          <span className="max-w-full truncate text-body font-medium">{contact.name}</span>
          {contact.note === null ? null : (
            <TooltipText
              content={contact.note}
              render={<span className="min-w-0 truncate text-caption text-fg-faint" />}
            >
              {contact.note}
            </TooltipText>
          )}
          <RowActions>
            <CopyButton value={contact.address} />
            <Menu>
              <MenuTrigger render={<Button variant="ghost" size="icon-xs" />}>
                <Icon name="more" />
              </MenuTrigger>
              <MenuPopup align="end">
                <MenuGroup>
                  <MenuItem onClick={onEdit}>
                    <Icon name="rename" />
                    Edit...
                  </MenuItem>
                  <MenuItem tone="danger" onClick={onRemove}>
                    <Icon name="remove" />
                    Remove
                  </MenuItem>
                </MenuGroup>
              </MenuPopup>
            </Menu>
          </RowActions>
        </span>
        <Identifier
          value={contact.address}
          isTruncated
          className="font-mono text-caption text-fg-muted"
        />
      </span>
    </Row>
  )
}

const NEW_DESCRIPTION = "Save an address you send to, then pick it from the recipient field."

const DUPLICATE_ERROR = "This address is already in your address book."

interface ContactDialogProps extends DialogStateProps {
  contact: ContactRecord | null
  chains: readonly ChainId[]
  defaultChain: ChainId
}

function ContactDialog({
  open,
  onOpenChange,
  onOpenChangeComplete,
  contact,
  chains,
  defaultChain,
}: ContactDialogProps) {
  const contacts = useQuery(contactQueries.all())
  const [chain, setChain] = useState<ChainId>(contact?.chain ?? defaultChain)
  const [name, setName] = useState(contact?.name ?? "")
  const [address, setAddress] = useState(contact?.address ?? "")
  const [note, setNote] = useState(contact?.note ?? "")

  const trimmedName = name.trim()
  const trimmedAddress = address.trim()
  const noteArg = trimmedOrNull(note)

  const deferredAddress = useDeferredValue(trimmedAddress)
  const validation = useQuery(addressQueries.validate(chain, deferredAddress))
  const canonical = deferredAddress === trimmedAddress ? (validation.data ?? null) : null
  const isDuplicate =
    canonical !== null &&
    (contacts.data ?? []).some(
      (row) => row.id !== contact?.id && row.chain === chain && row.address === canonical,
    )

  let addressError: string | null = null
  if (isDuplicate) {
    addressError = DUPLICATE_ERROR
  } else if (validation.isError) {
    addressError = describeFailure(validation.error)
  }

  const isUnchanged =
    canonical === contact?.address && trimmedName === contact.name && noteArg === contact.note
  const canSave = trimmedName !== "" && canonical !== null && !isDuplicate && !isUnchanged

  const save = useIpcMutation<null>({
    mutationFn: () =>
      contact === null
        ? contactCommands.add(chain, trimmedAddress, trimmedName, noteArg)
        : contactCommands.update(contact.id, trimmedAddress, trimmedName, noteArg),
    errorTitle: contact === null ? "Could not save the contact" : "Could not update the contact",
    invalidates: [contactKeys.root],
    successToast: contact === null ? "Contact saved" : "Contact updated",
    onDone: () => {
      onOpenChange(false)
    },
  })

  let saveError: string | null = null
  if (save.error !== null) {
    saveError = hasCode(save.error, "already_exists")
      ? DUPLICATE_ERROR
      : describeFailure(save.error)
  }

  return (
    <FormDialog
      open={open}
      onOpenChange={onOpenChange}
      onOpenChangeComplete={onOpenChangeComplete}
      title={contact === null ? "New contact" : "Edit contact"}
      description={contact === null ? NEW_DESCRIPTION : undefined}
      submitLabel={contact === null ? "Add" : "Save"}
      isSubmitDisabled={!canSave}
      isBusy={save.isPending}
      error={saveError}
      onSubmit={() => {
        save.mutate()
      }}
    >
      <div className="flex gap-3">
        <Field label="Name" className="min-w-0 flex-1">
          <Input
            autoFocus
            autoComplete="off"
            value={name}
            onChange={(e) => {
              setName(e.target.value)
            }}
          />
        </Field>
        {contact === null ? (
          <Field label="Chain" className="w-36 shrink-0">
            <Select<ChainId> value={chain} onValueChange={setChain}>
              <SelectTrigger className="w-full">
                <ChainLogo chain={chain} />
                {chainName(chain)}
              </SelectTrigger>
              <SelectPopup>
                {chains.map((option) => (
                  <SelectItem key={option} value={option}>
                    <ChainLogo chain={option} />
                    {chainName(option)}
                  </SelectItem>
                ))}
              </SelectPopup>
            </Select>
          </Field>
        ) : null}
      </div>

      <Field label={`${chainName(chain)} address`} error={addressError}>
        <Input
          isMono
          autoComplete="off"
          spellCheck={false}
          value={address}
          attachment={
            canonical === null || isDuplicate ? null : <Icon name="check" className="text-ok" />
          }
          onChange={(e) => {
            setAddress(e.target.value)
          }}
        />
      </Field>

      <Field label="Optional note">
        <Input
          autoComplete="off"
          value={note}
          onChange={(e) => {
            setNote(e.target.value)
          }}
        />
      </Field>
    </FormDialog>
  )
}

const SKELETON_ROWS = 4

interface ContactDialogPayload {
  contact: ContactRecord | null
}

function matchesSearch(contact: ContactRecord, query: string): boolean {
  return [contact.name, contact.note ?? "", contact.address, chainName(contact.chain)].some(
    (text) => text.toLowerCase().includes(query),
  )
}

export function ContactsView() {
  const chains = useSuspenseQuery(chainQueries.enabled()).data
  const contacts = useQuery(contactQueries.all())
  const [search, setSearch] = useState("")
  const dialog = useDialogPayload<ContactDialogPayload>()

  const remove = useIpcMutation<null, ContactRecord>({
    mutationFn: (contact) => contactCommands.remove(contact.id),
    errorTitle: "Could not remove the contact",
    invalidates: [contactKeys.root],
    successToast: (_outcome, contact) => `Removed ${contact.name}`,
  })

  const rows = contacts.data ?? []
  const query = search.trim().toLowerCase()
  const matchingContacts =
    query === "" ? rows : rows.filter((contact) => matchesSearch(contact, query))
  const groups = allChains()
    .map((chain) => ({
      chain,
      members: matchingContacts.filter((contact) => contact.chain === chain),
    }))
    .filter((group) => group.members.length > 0)
  const defaultChain = chains.at(0)

  const addButton = (
    <Button
      size="sm"
      disabled={defaultChain === undefined}
      onClick={() => {
        dialog.show({ contact: null })
      }}
    >
      <Icon name="add" />
      Add contact
    </Button>
  )

  return (
    <Panel className="min-w-0 flex-1">
      <PanelHeader
        title="Address book"
        count={rows.length}
        actions={
          rows.length === 0 ? undefined : (
            <>
              <ContactSearch value={search} onValueChange={setSearch} />
              {addButton}
            </>
          )
        }
      />

      <PanelBody className="flex flex-col gap-1">
        <ListStatus
          state={contacts}
          isEmpty={rows.length === 0}
          errorTitle="Could not load the address book"
          emptyTitle="No contacts yet"
          emptyDescription="Save the addresses you send to, then pick them from the recipient field when you send."
          emptyAction={addButton}
          skeletonRowCount={SKELETON_ROWS}
          emptyClassName="flex-1"
        >
          {groups.length === 0 ? (
            <EmptyState
              className="flex-1"
              title="No contacts match your search"
              description="Search by name, note, address, or chain."
            />
          ) : (
            groups.map((group) => (
              <section key={group.chain}>
                <h3 className="flex items-center gap-2 px-2 pt-2 pb-1.5 text-caption font-medium text-fg-muted">
                  <ChainLogo chain={group.chain} className="size-icon-md" />
                  {chainName(group.chain)}
                  <span className="text-fg-faint tabular-nums">{group.members.length}</span>
                </h3>
                <div className="grid grid-cols-[repeat(auto-fill,minmax(--spacing(72),1fr))] gap-1.5">
                  {group.members.map((contact) => (
                    <ContactTile
                      key={contact.id}
                      contact={contact}
                      onEdit={() => {
                        dialog.show({ contact })
                      }}
                      onRemove={() => {
                        remove.mutate(contact)
                      }}
                    />
                  ))}
                </div>
              </section>
            ))
          )}
        </ListStatus>
      </PanelBody>

      {dialog.payload === null || defaultChain === undefined ? null : (
        <ContactDialog
          key={dialog.key}
          {...dialog.props}
          contact={dialog.payload.contact}
          chains={chains}
          defaultChain={defaultChain}
        />
      )}
    </Panel>
  )
}
