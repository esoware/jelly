import { useSuspenseQuery } from "@tanstack/react-query"

import type { FiatCurrency } from "@/ipc/bindings.gen.ts"
import { CURRENCIES } from "@/lib/fiat.ts"
import { loadPrices } from "@/query/money.ts"
import { useIpcMutation } from "@/query/mutation.ts"
import { patchSettings, settingCommands, settingKeys, settingQueries } from "@/query/setting.ts"
import { resetPriceHistory, resetPrices } from "@/store.ts"
import { Field } from "@/ui/field.tsx"
import { SectionHeader } from "@/ui/section-header.tsx"
import { Select, SelectPopup, SelectItem, SelectTrigger } from "@/ui/select.tsx"
import { Switch } from "@/ui/switch.tsx"
import { CurrencyFlag } from "@/widgets/logos.tsx"

function PriceFeedToggle() {
  const settings = useSuspenseQuery(settingQueries.all())
  const { isPriceFeedEnabled, isPriceChartEnabled } = settings.data

  const save = useIpcMutation<null, boolean>({
    mutationFn: settingCommands.setPriceFeedEnabled,
    invalidates: [settingKeys.root],
    optimistic: (client, isEnabled) => {
      patchSettings(client, {
        isPriceFeedEnabled: isEnabled,
        amountEntry: isEnabled ? "fiat" : "units",
      })
    },
    errorTitle: "Could not change the price feed",
    onDone: (_data, isEnabled) => {
      if (isEnabled) {
        void loadPrices()
      } else {
        resetPrices()
        resetPriceHistory()
      }
    },
  })

  const saveCharts = useIpcMutation<null, boolean>({
    mutationFn: settingCommands.setPriceChartEnabled,
    invalidates: [settingKeys.root],
    optimistic: (client, isEnabled) => {
      patchSettings(client, { isPriceChartEnabled: isEnabled })
    },
    errorTitle: "Could not change the price charts",
  })

  return (
    <div className="space-y-3 pt-3">
      <SectionHeader
        level="subsection"
        title="Price feed"
        description="Show what your holdings are worth in your currency. Jelly never sends your addresses or balances to the price provider."
      />

      <label className="flex items-center gap-2 text-body">
        <Switch
          checked={isPriceFeedEnabled}
          onCheckedChange={(isEnabled) => {
            save.mutate(isEnabled)
          }}
        />
        Show fiat values
      </label>

      <label
        data-disabled={!isPriceFeedEnabled}
        className="flex items-center gap-2 text-body data-[disabled=true]:text-fg-faint"
      >
        <Switch
          checked={isPriceChartEnabled}
          disabled={!isPriceFeedEnabled}
          onCheckedChange={(isEnabled) => {
            saveCharts.mutate(isEnabled)
          }}
        />
        Show price charts
      </label>
    </div>
  )
}

const DISPLAY_ORDER: { readonly [K in FiatCurrency]: K } = {
  usd: "usd",
  eur: "eur",
  gbp: "gbp",
  jpy: "jpy",
  chf: "chf",
  cad: "cad",
  aud: "aud",
}

const LISTED_CURRENCIES = Object.values(DISPLAY_ORDER)

export function CurrencyView() {
  const settings = useSuspenseQuery(settingQueries.all())
  const selected = settings.data.fiatCurrency

  const save = useIpcMutation<null, FiatCurrency>({
    mutationFn: settingCommands.setFiatCurrency,
    invalidates: [settingKeys.root],
    optimistic: (client, fiatCurrency) => {
      patchSettings(client, { fiatCurrency })
    },
    errorTitle: "Could not change the currency",
  })

  return (
    <>
      <SectionHeader
        level="page"
        title="Currency"
        description="Choose the currency Jelly shows values in."
      />

      <div className="space-y-3">
        <Field
          label="Currency"
          hint="Prices come from Kraken in US dollars and are converted at its dollar exchange rate."
        >
          <Select
            value={selected}
            onValueChange={(currency) => {
              save.mutate(currency)
            }}
          >
            <SelectTrigger className="self-start">
              <CurrencyFlag currency={selected} />
              <span>{CURRENCIES[selected].name}</span>
              <span className="text-fg-faint">{CURRENCIES[selected].code}</span>
            </SelectTrigger>
            <SelectPopup>
              {LISTED_CURRENCIES.map((currency) => (
                <SelectItem key={currency} value={currency} description={CURRENCIES[currency].code}>
                  <CurrencyFlag currency={currency} />
                  {CURRENCIES[currency].name}
                </SelectItem>
              ))}
            </SelectPopup>
          </Select>
        </Field>

        <PriceFeedToggle />
      </div>
    </>
  )
}
