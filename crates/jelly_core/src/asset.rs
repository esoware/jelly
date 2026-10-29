use serde::{Deserialize, Serialize};

use crate::chain::ChainId;
use crate::coin::Coin;
use crate::wire_enum;

wire_enum! {
    #[cfg_attr(feature = "ipc", derive(specta::Type))]
    /// Supported token contracts and mints.
    ///
    /// Each chain's tokens sit together in chain order, and the wallet lists them as
    /// declared: other tokens first, then real-world assets with the dollar coins before
    /// gold, each group by market capitalization.
    pub enum TokenId {
        LinkErc20 => "link_erc20",
        WbtcErc20 => "wbtc_erc20",
        WethErc20 => "weth_erc20",
        UniErc20 => "uni_erc20",
        ShibErc20 => "shib_erc20",
        EnaErc20 => "ena_erc20",
        AaveErc20 => "aave_erc20",
        OndoErc20 => "ondo_erc20",
        PepeErc20 => "pepe_erc20",
        ArbErc20 => "arb_erc20",
        UsdtErc20 => "usdt_erc20",
        UsdcErc20 => "usdc_erc20",
        UsdsErc20 => "usds_erc20",
        DaiErc20 => "dai_erc20",
        UsdgErc20 => "usdg_erc20",
        PyusdErc20 => "pyusd_erc20",
        XautErc20 => "xaut_erc20",
        PaxgErc20 => "paxg_erc20",
        LinkArbitrum => "link_arbitrum",
        WethArbitrum => "weth_arbitrum",
        EnaArbitrum => "ena_arbitrum",
        ArbArbitrum => "arb_arbitrum",
        UsdtArbitrum => "usdt_arbitrum",
        UsdcArbitrum => "usdc_arbitrum",
        UsdsArbitrum => "usds_arbitrum",
        UsdgArbitrum => "usdg_arbitrum",
        PyusdArbitrum => "pyusd_arbitrum",
        XautArbitrum => "xaut_arbitrum",
        LinkBase => "link_base",
        WethBase => "weth_base",
        EnaBase => "ena_base",
        UsdcBase => "usdc_base",
        UsdsBase => "usds_base",
        LinkOptimism => "link_optimism",
        WethOptimism => "weth_optimism",
        EnaOptimism => "ena_optimism",
        OpOptimism => "op_optimism",
        UsdtOptimism => "usdt_optimism",
        UsdcOptimism => "usdc_optimism",
        UsdsOptimism => "usds_optimism",
        LinkBep20 => "link_bep20",
        WethBep20 => "weth_bep20",
        EnaBep20 => "ena_bep20",
        UsdtBep20 => "usdt_bep20",
        UsdcBep20 => "usdc_bep20",
        XautBep20 => "xaut_bep20",
        LinkPolygon => "link_polygon",
        WethPolygon => "weth_polygon",
        UsdtPolygon => "usdt_polygon",
        UsdcPolygon => "usdc_polygon",
        PyusdPolygon => "pyusd_polygon",
        XautPolygon => "xaut_polygon",
        LinkAvalanche => "link_avalanche",
        WethAvalanche => "weth_avalanche",
        UsdtAvalanche => "usdt_avalanche",
        UsdcAvalanche => "usdc_avalanche",
        UsdsAvalanche => "usds_avalanche",
        XautAvalanche => "xaut_avalanche",
        LinkSpl => "link_spl",
        WbtcSpl => "wbtc_spl",
        EnaSpl => "ena_spl",
        PumpSpl => "pump_spl",
        JupSpl => "jup_spl",
        UsdtSpl => "usdt_spl",
        UsdcSpl => "usdc_spl",
        UsdsSpl => "usds_spl",
        UsdgSpl => "usdg_spl",
        PyusdSpl => "pyusd_spl",
        XautSpl => "xaut_spl",
        PaxgSpl => "paxg_spl",
        WbtcTrc20 => "wbtc_trc20",
        UsdtTrc20 => "usdt_trc20",
        EnaJetton => "ena_jetton",
        UsdtJetton => "usdt_jetton",
        XautJetton => "xaut_jetton",
    }
}

/// Ethena deploys its omnichain ENA at this address on Arbitrum, Base, Optimism, and BNB
/// Smart Chain.
const LAYERZERO_ENA: &str = "0x58538e6A46E07434d7E7375Bc268D3cb839C0133";

/// Every OP Stack network predeploys WETH at this address.
const OP_STACK_WETH: &str = "0x4200000000000000000000000000000000000006";

struct Deployment {
    /// What the token reads as, whatever its contract calls itself: Tether's omnichain
    /// `USDT0` and `XAUt0` and bridged copies such as Avalanche's `LINK.e` take the coin
    /// they stand for, and the ether Binance issues on BNB Smart Chain as `ETH` reads as
    /// WETH, like every other network's.
    coin: Coin,
    chain: ChainId,
    decimals: u8,
    contract: &'static str,
}

impl TokenId {
    pub const fn coin(self) -> Coin {
        self.deployment().coin
    }

    pub const fn chain(self) -> ChainId {
        self.deployment().chain
    }

    pub const fn decimals(self) -> u8 {
        self.deployment().decimals
    }

    /// Mainnet contract address, SPL mint, or jetton master.
    pub const fn contract(self) -> &'static str {
        self.deployment().contract
    }

    #[expect(clippy::too_many_lines, reason = "a data table with one row per token")]
    const fn deployment(self) -> Deployment {
        match self {
            Self::LinkErc20 => Deployment {
                coin: Coin::Link,
                chain: ChainId::Ethereum,
                decimals: 18,
                contract: "0x514910771AF9Ca656af840dff83E8264EcF986CA",
            },
            Self::WbtcErc20 => Deployment {
                coin: Coin::Wbtc,
                chain: ChainId::Ethereum,
                decimals: 8,
                contract: "0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599",
            },
            Self::WethErc20 => Deployment {
                coin: Coin::Weth,
                chain: ChainId::Ethereum,
                decimals: 18,
                contract: "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2",
            },
            Self::UniErc20 => Deployment {
                coin: Coin::Uni,
                chain: ChainId::Ethereum,
                decimals: 18,
                contract: "0x1f9840a85d5aF5bf1D1762F925BDADdC4201F984",
            },
            Self::ShibErc20 => Deployment {
                coin: Coin::Shib,
                chain: ChainId::Ethereum,
                decimals: 18,
                contract: "0x95aD61b0a150d79219dCF64E1E6Cc01f0B64C4cE",
            },
            Self::EnaErc20 => Deployment {
                coin: Coin::Ena,
                chain: ChainId::Ethereum,
                decimals: 18,
                contract: "0x57e114B691Db790C35207b2e685D4A43181e6061",
            },
            Self::AaveErc20 => Deployment {
                coin: Coin::Aave,
                chain: ChainId::Ethereum,
                decimals: 18,
                contract: "0x7Fc66500c84A76Ad7e9c93437bFc5Ac33E2DDaE9",
            },
            Self::OndoErc20 => Deployment {
                coin: Coin::Ondo,
                chain: ChainId::Ethereum,
                decimals: 18,
                contract: "0xfAbA6f8e4a5E8Ab82F62fe7C39859FA577269BE3",
            },
            Self::PepeErc20 => Deployment {
                coin: Coin::Pepe,
                chain: ChainId::Ethereum,
                decimals: 18,
                contract: "0x6982508145454Ce325dDbE47a25d4ec3d2311933",
            },
            Self::ArbErc20 => Deployment {
                coin: Coin::Arb,
                chain: ChainId::Ethereum,
                decimals: 18,
                contract: "0xB50721BCf8d664c30412Cfbc6cf7a15145234ad1",
            },
            Self::UsdtErc20 => Deployment {
                coin: Coin::Usdt,
                chain: ChainId::Ethereum,
                decimals: 6,
                contract: "0xdAC17F958D2ee523a2206206994597C13D831ec7",
            },
            Self::UsdcErc20 => Deployment {
                coin: Coin::Usdc,
                chain: ChainId::Ethereum,
                decimals: 6,
                contract: "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48",
            },
            Self::UsdsErc20 => Deployment {
                coin: Coin::Usds,
                chain: ChainId::Ethereum,
                decimals: 18,
                contract: "0xdC035D45d973E3EC169d2276DDab16f1e407384F",
            },
            Self::DaiErc20 => Deployment {
                coin: Coin::Dai,
                chain: ChainId::Ethereum,
                decimals: 18,
                contract: "0x6B175474E89094C44Da98b954EedeAC495271d0F",
            },
            Self::UsdgErc20 => Deployment {
                coin: Coin::Usdg,
                chain: ChainId::Ethereum,
                decimals: 6,
                contract: "0xe343167631d89B6Ffc58B88d6b7fB0228795491D",
            },
            Self::PyusdErc20 => Deployment {
                coin: Coin::Pyusd,
                chain: ChainId::Ethereum,
                decimals: 6,
                contract: "0x6c3ea9036406852006290770BEdFcAbA0e23A0e8",
            },
            Self::XautErc20 => Deployment {
                coin: Coin::Xaut,
                chain: ChainId::Ethereum,
                decimals: 6,
                contract: "0x68749665FF8D2d112Fa859AA293F07A622782F38",
            },
            Self::PaxgErc20 => Deployment {
                coin: Coin::Paxg,
                chain: ChainId::Ethereum,
                decimals: 18,
                contract: "0x45804880De22913dAFE09f4980848ECE6EcbAf78",
            },
            Self::LinkArbitrum => Deployment {
                coin: Coin::Link,
                chain: ChainId::Arbitrum,
                decimals: 18,
                contract: "0xf97f4df75117a78c1A5a0DBb814Af92458539FB4",
            },
            Self::WethArbitrum => Deployment {
                coin: Coin::Weth,
                chain: ChainId::Arbitrum,
                decimals: 18,
                contract: "0x82aF49447D8a07e3bd95BD0d56f35241523fBab1",
            },
            Self::EnaArbitrum => Deployment {
                coin: Coin::Ena,
                chain: ChainId::Arbitrum,
                decimals: 18,
                contract: LAYERZERO_ENA,
            },
            Self::ArbArbitrum => Deployment {
                coin: Coin::Arb,
                chain: ChainId::Arbitrum,
                decimals: 18,
                contract: "0x912CE59144191C1204E64559FE8253a0e49E6548",
            },
            Self::UsdtArbitrum => Deployment {
                coin: Coin::Usdt,
                chain: ChainId::Arbitrum,
                decimals: 6,
                contract: "0xFd086bC7CD5C481DCC9C85ebE478A1C0b69FCbb9",
            },
            Self::UsdcArbitrum => Deployment {
                coin: Coin::Usdc,
                chain: ChainId::Arbitrum,
                decimals: 6,
                contract: "0xaf88d065e77c8cC2239327C5EDb3A432268e5831",
            },
            Self::UsdsArbitrum => Deployment {
                coin: Coin::Usds,
                chain: ChainId::Arbitrum,
                decimals: 18,
                contract: "0x6491c05A82219b8D1479057361ff1654749b876b",
            },
            Self::UsdgArbitrum => Deployment {
                coin: Coin::Usdg,
                chain: ChainId::Arbitrum,
                decimals: 6,
                contract: "0x004B506865409877C9fA29bfb1ebA929984B9bbC",
            },
            Self::PyusdArbitrum => Deployment {
                coin: Coin::Pyusd,
                chain: ChainId::Arbitrum,
                decimals: 6,
                contract: "0x46850aD61C2B7d64d08c9C754F45254596696984",
            },
            Self::XautArbitrum => Deployment {
                coin: Coin::Xaut,
                chain: ChainId::Arbitrum,
                decimals: 6,
                contract: "0x40461291347e1eCbb09499F3371D3f17f10d7159",
            },
            Self::LinkBase => Deployment {
                coin: Coin::Link,
                chain: ChainId::Base,
                decimals: 18,
                contract: "0x88Fb150BDc53A65fe94Dea0c9BA0a6dAf8C6e196",
            },
            Self::WethBase => Deployment {
                coin: Coin::Weth,
                chain: ChainId::Base,
                decimals: 18,
                contract: OP_STACK_WETH,
            },
            Self::EnaBase => Deployment {
                coin: Coin::Ena,
                chain: ChainId::Base,
                decimals: 18,
                contract: LAYERZERO_ENA,
            },
            Self::UsdcBase => Deployment {
                coin: Coin::Usdc,
                chain: ChainId::Base,
                decimals: 6,
                contract: "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913",
            },
            Self::UsdsBase => Deployment {
                coin: Coin::Usds,
                chain: ChainId::Base,
                decimals: 18,
                contract: "0x820C137fa70C8691f0e44Dc420a5e53c168921Dc",
            },
            Self::LinkOptimism => Deployment {
                coin: Coin::Link,
                chain: ChainId::Optimism,
                decimals: 18,
                contract: "0x350a791Bfc2C21F9Ed5d10980Dad2e2638ffa7f6",
            },
            Self::WethOptimism => Deployment {
                coin: Coin::Weth,
                chain: ChainId::Optimism,
                decimals: 18,
                contract: OP_STACK_WETH,
            },
            Self::EnaOptimism => Deployment {
                coin: Coin::Ena,
                chain: ChainId::Optimism,
                decimals: 18,
                contract: LAYERZERO_ENA,
            },
            Self::OpOptimism => Deployment {
                coin: Coin::Op,
                chain: ChainId::Optimism,
                decimals: 18,
                contract: "0x4200000000000000000000000000000000000042",
            },
            Self::UsdtOptimism => Deployment {
                coin: Coin::Usdt,
                chain: ChainId::Optimism,
                decimals: 6,
                contract: "0x01bFF41798a0BcF287b996046Ca68b395DbC1071",
            },
            Self::UsdcOptimism => Deployment {
                coin: Coin::Usdc,
                chain: ChainId::Optimism,
                decimals: 6,
                contract: "0x0b2C639c533813f4Aa9D7837CAf62653d097Ff85",
            },
            Self::UsdsOptimism => Deployment {
                coin: Coin::Usds,
                chain: ChainId::Optimism,
                decimals: 18,
                contract: "0x4F13a96EC5C4Cf34e442b46Bbd98a0791F20edC3",
            },
            Self::LinkBep20 => Deployment {
                coin: Coin::Link,
                chain: ChainId::Bsc,
                decimals: 18,
                contract: "0xF8A0BF9cF54Bb92F17374d9e9A321E6a111a51bD",
            },
            Self::WethBep20 => Deployment {
                coin: Coin::Weth,
                chain: ChainId::Bsc,
                decimals: 18,
                contract: "0x2170Ed0880ac9A755fd29B2688956BD959F933F8",
            },
            Self::EnaBep20 => Deployment {
                coin: Coin::Ena,
                chain: ChainId::Bsc,
                decimals: 18,
                contract: LAYERZERO_ENA,
            },
            // Binance-Peg USDT and USDC carry 18 decimals, unlike their Ethereum originals.
            Self::UsdtBep20 => Deployment {
                coin: Coin::Usdt,
                chain: ChainId::Bsc,
                decimals: 18,
                contract: "0x55d398326f99059fF775485246999027B3197955",
            },
            Self::UsdcBep20 => Deployment {
                coin: Coin::Usdc,
                chain: ChainId::Bsc,
                decimals: 18,
                contract: "0x8AC76a51cc950d9822D68b83fE1Ad97B32Cd580d",
            },
            Self::XautBep20 => Deployment {
                coin: Coin::Xaut,
                chain: ChainId::Bsc,
                decimals: 6,
                contract: "0x21cAef8A43163Eea865baeE23b9C2E327696A3bf",
            },
            Self::LinkPolygon => Deployment {
                coin: Coin::Link,
                chain: ChainId::Polygon,
                decimals: 18,
                contract: "0x53E0bca35eC356BD5ddDFebbD1Fc0fD03FaBad39",
            },
            Self::WethPolygon => Deployment {
                coin: Coin::Weth,
                chain: ChainId::Polygon,
                decimals: 18,
                contract: "0x7ceB23fD6bC0adD59E62ac25578270cFf1b9f619",
            },
            Self::UsdtPolygon => Deployment {
                coin: Coin::Usdt,
                chain: ChainId::Polygon,
                decimals: 6,
                contract: "0xc2132D05D31c914a87C6611C10748AEb04B58e8F",
            },
            Self::UsdcPolygon => Deployment {
                coin: Coin::Usdc,
                chain: ChainId::Polygon,
                decimals: 6,
                contract: "0x3c499c542cEF5E3811e1192ce70d8cC03d5c3359",
            },
            Self::PyusdPolygon => Deployment {
                coin: Coin::Pyusd,
                chain: ChainId::Polygon,
                decimals: 6,
                contract: "0x99aF3EeA856556646C98c8B9b2548Fe815240750",
            },
            Self::XautPolygon => Deployment {
                coin: Coin::Xaut,
                chain: ChainId::Polygon,
                decimals: 6,
                contract: "0xF1815bd50389c46847f0Bda824eC8da914045D14",
            },
            Self::LinkAvalanche => Deployment {
                coin: Coin::Link,
                chain: ChainId::Avalanche,
                decimals: 18,
                contract: "0x5947BB275c521040051D82396192181b413227A3",
            },
            Self::WethAvalanche => Deployment {
                coin: Coin::Weth,
                chain: ChainId::Avalanche,
                decimals: 18,
                contract: "0x49D5c2BdFfac6CE2BFdB6640F4F80f226bc10bAB",
            },
            Self::UsdtAvalanche => Deployment {
                coin: Coin::Usdt,
                chain: ChainId::Avalanche,
                decimals: 6,
                contract: "0x9702230A8Ea53601f5cD2dc00fDBc13d4dF4A8c7",
            },
            Self::UsdcAvalanche => Deployment {
                coin: Coin::Usdc,
                chain: ChainId::Avalanche,
                decimals: 6,
                contract: "0xB97EF9Ef8734C71904D8002F8b6Bc66Dd9c48a6E",
            },
            Self::UsdsAvalanche => Deployment {
                coin: Coin::Usds,
                chain: ChainId::Avalanche,
                decimals: 18,
                contract: "0x86Ff09db814ac346a7C6FE2Cd648F27706D1D470",
            },
            Self::XautAvalanche => Deployment {
                coin: Coin::Xaut,
                chain: ChainId::Avalanche,
                decimals: 6,
                contract: "0x2775d5105276781B4b85bA6eA6a6653bEeD1dd32",
            },
            Self::LinkSpl => Deployment {
                coin: Coin::Link,
                chain: ChainId::Solana,
                decimals: 9,
                contract: "LinkhB3afbBKb2EQQu7s7umdZceV3wcvAUJhQAfQ23L",
            },
            Self::WbtcSpl => Deployment {
                coin: Coin::Wbtc,
                chain: ChainId::Solana,
                decimals: 8,
                contract: "5XZw2LKTyrfvfiskJ78AMpackRjPcyCif1WhUsPDuVqQ",
            },
            Self::EnaSpl => Deployment {
                coin: Coin::Ena,
                chain: ChainId::Solana,
                decimals: 9,
                contract: "72QvBVwpxqmheEPfaCwWSWqEFsUy3rhWt6JhQBMNTwD1",
            },
            Self::PumpSpl => Deployment {
                coin: Coin::Pump,
                chain: ChainId::Solana,
                decimals: 6,
                contract: "pumpCmXqMfrsAkQ5r49WcJnRayYRqmXz6ae8H7H9Dfn",
            },
            Self::JupSpl => Deployment {
                coin: Coin::Jup,
                chain: ChainId::Solana,
                decimals: 6,
                contract: "JUPyiwrYJFskUPiHa7hkeR8VUtAeFoSYbKedZNsDvCN",
            },
            Self::UsdtSpl => Deployment {
                coin: Coin::Usdt,
                chain: ChainId::Solana,
                decimals: 6,
                contract: "Es9vMFrzaCERmJfrF4H2FYD4KCoNkY11McCe8BenwNYB",
            },
            Self::UsdcSpl => Deployment {
                coin: Coin::Usdc,
                chain: ChainId::Solana,
                decimals: 6,
                contract: "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
            },
            Self::UsdsSpl => Deployment {
                coin: Coin::Usds,
                chain: ChainId::Solana,
                decimals: 6,
                contract: "USDSwr9ApdHk5bvJKMjzff41FfuX8bSxdKcR81vTwcA",
            },
            Self::UsdgSpl => Deployment {
                coin: Coin::Usdg,
                chain: ChainId::Solana,
                decimals: 6,
                contract: "2u1tszSeqZ3qBWF3uNGPFc8TzMk2tdiwknnRMWGWjGWH",
            },
            Self::PyusdSpl => Deployment {
                coin: Coin::Pyusd,
                chain: ChainId::Solana,
                decimals: 6,
                contract: "2b1kV6DkPAnxd5ixfnxCpjxmKwqjjaYmCZfHsFu24GXo",
            },
            Self::XautSpl => Deployment {
                coin: Coin::Xaut,
                chain: ChainId::Solana,
                decimals: 6,
                contract: "AymATz4TCL9sWNEEV9Kvyz45CHVhDZ6kUgjTJPzLpU9P",
            },
            Self::PaxgSpl => Deployment {
                coin: Coin::Paxg,
                chain: ChainId::Solana,
                decimals: 6,
                contract: "5GgRAEmv8ZxF2PR5hY72Qs5x1bnQ6UK2RbTPoqJ3wSwW",
            },
            Self::WbtcTrc20 => Deployment {
                coin: Coin::Wbtc,
                chain: ChainId::Tron,
                decimals: 8,
                contract: "TYhWwKpw43ENFWBTGpzLHn3882f2au7SMi",
            },
            Self::UsdtTrc20 => Deployment {
                coin: Coin::Usdt,
                chain: ChainId::Tron,
                decimals: 6,
                contract: "TR7NHqjeKQxGTCi8q8ZY4pL8otSzgjLj6t",
            },
            Self::EnaJetton => Deployment {
                coin: Coin::Ena,
                chain: ChainId::Ton,
                decimals: 6,
                contract: "EQAFhZFZYye0llsl785OzOJG9QT6PXyYUdmiC4U95lBuHh1S",
            },
            Self::UsdtJetton => Deployment {
                coin: Coin::Usdt,
                chain: ChainId::Ton,
                decimals: 6,
                contract: "EQCxE6mUtQJKFnGfaROTKOt1lZbDiiX1kCixRv7Nw2Id_sDs",
            },
            Self::XautJetton => Deployment {
                coin: Coin::Xaut,
                chain: ChainId::Ton,
                decimals: 6,
                contract: "EQA1R_LuQCLHlMgOo1S4G7Y7W1cd0FrAkbA10Zq7rddKxi9k",
            },
        }
    }
}

/// A native coin or supported token. Decimals belong to the asset, while `Amount` stores
/// base units.
#[cfg_attr(feature = "ipc", derive(specta::Type))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum AssetId {
    Native(ChainId),
    Token(TokenId),
}

impl AssetId {
    #[cfg(feature = "test-util")]
    pub const COUNT: usize = ChainId::COUNT + TokenId::COUNT;

    /// Every native coin in [`ChainId::ALL`] order, then every token in [`TokenId::ALL`]
    /// order.
    pub fn all() -> impl Iterator<Item = Self> {
        ChainId::ALL.into_iter().map(Self::Native).chain(TokenId::ALL.into_iter().map(Self::Token))
    }

    pub const fn chain(self) -> ChainId {
        match self {
            Self::Native(chain) => chain,
            Self::Token(token) => token.chain(),
        }
    }

    pub const fn decimals(self) -> u8 {
        match self {
            Self::Native(chain) => chain.native_decimals(),
            Self::Token(token) => token.decimals(),
        }
    }

    pub const fn coin(self) -> Coin {
        match self {
            Self::Native(chain) => chain.coin(),
            Self::Token(token) => token.coin(),
        }
    }

    /// The stable `<kind>:<name>` string the store's `asset` columns hold.
    pub fn key(self) -> String {
        match self {
            Self::Native(chain) => format!("native:{}", chain.as_str()),
            Self::Token(token) => format!("token:{}", token.as_str()),
        }
    }

    /// The inverse of [`AssetId::key`].
    pub fn parse_key(key: &str) -> Option<Self> {
        match key.split_once(':')? {
            ("native", chain) => ChainId::parse(chain).map(Self::Native),
            ("token", token) => TokenId::parse(token).map(Self::Token),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn every_asset_has_a_unique_stable_key_that_round_trips() {
        let mut seen = BTreeSet::new();
        for asset in AssetId::all() {
            let key = asset.key();
            assert!(seen.insert(key.clone()), "two assets spell themselves {key}");
            assert_eq!(AssetId::parse_key(&key), Some(asset), "{key}");
        }
        assert_eq!(AssetId::Native(ChainId::Bitcoin).key(), "native:bitcoin");
        assert_eq!(AssetId::Token(TokenId::UsdtErc20).key(), "token:usdt_erc20");
        assert_eq!(AssetId::parse_key("native:namecoin"), None);
        assert_eq!(AssetId::parse_key("usdt_erc20"), None);
        assert_eq!(AssetId::parse_key(""), None);
    }

    #[test]
    fn an_asset_takes_its_chain_scale_and_coin_from_its_chain_or_token() {
        for asset in AssetId::all() {
            match asset {
                AssetId::Native(chain) => {
                    assert_eq!(asset.chain(), chain);
                    assert_eq!(asset.decimals(), chain.native_decimals());
                    assert_eq!(asset.coin(), chain.coin());
                }
                AssetId::Token(token) => {
                    assert_eq!(asset.chain(), token.chain());
                    assert_eq!(asset.decimals(), token.decimals());
                    assert_eq!(asset.coin(), token.coin());
                }
            }
        }
    }

    #[test]
    fn every_token_carries_its_network_specific_decimals() {
        assert_eq!(TokenId::UsdtBep20.decimals(), 18);
        assert_eq!(TokenId::UsdcBep20.decimals(), 18);
        assert_eq!(TokenId::LinkSpl.decimals(), 9, "Solana's LINK is scaled for its own mint");
        assert_eq!(TokenId::EnaSpl.decimals(), 9, "so is Solana's ENA");
        assert_eq!(TokenId::EnaJetton.decimals(), 6, "and TON's ENA has six");
        assert_eq!(TokenId::WbtcErc20.decimals(), 8, "wrapped bitcoin keeps bitcoin's satoshi");
        for token in TokenId::ALL {
            let expected = match token.coin().symbol() {
                "LINK" | "ENA" if token.chain() == ChainId::Solana => 9,
                "ENA" if token.chain() == ChainId::Ton => 6,
                "WBTC" => 8,
                "LINK" | "WETH" | "UNI" | "SHIB" | "ENA" | "AAVE" | "ONDO" | "PEPE" | "ARB"
                | "OP" | "DAI" | "USDS" | "PAXG"
                    if token.chain() != ChainId::Solana =>
                {
                    18
                }
                "USDT" | "USDC" if token.chain() == ChainId::Bsc => 18,
                _ => 6,
            };
            assert_eq!(token.decimals(), expected, "{}", token.as_str());
        }
    }

    #[test]
    fn a_dollar_coin_is_the_same_coin_on_every_chain() {
        for token in [TokenId::UsdgErc20, TokenId::UsdgArbitrum, TokenId::UsdgSpl] {
            assert_eq!(token.coin(), Coin::Usdg, "{}", token.as_str());
        }
        for token in
            [TokenId::PyusdErc20, TokenId::PyusdPolygon, TokenId::PyusdArbitrum, TokenId::PyusdSpl]
        {
            assert_eq!(token.coin(), Coin::Pyusd, "{}", token.as_str());
        }
    }

    #[test]
    fn gold_tokens_keep_their_identity_and_network_specific_scale() {
        for (token, chain, coin, decimals) in [
            (TokenId::PaxgErc20, ChainId::Ethereum, Coin::Paxg, 18),
            (TokenId::PaxgSpl, ChainId::Solana, Coin::Paxg, 6),
            (TokenId::XautErc20, ChainId::Ethereum, Coin::Xaut, 6),
            (TokenId::XautArbitrum, ChainId::Arbitrum, Coin::Xaut, 6),
            (TokenId::XautBep20, ChainId::Bsc, Coin::Xaut, 6),
            (TokenId::XautPolygon, ChainId::Polygon, Coin::Xaut, 6),
            (TokenId::XautAvalanche, ChainId::Avalanche, Coin::Xaut, 6),
            (TokenId::XautSpl, ChainId::Solana, Coin::Xaut, 6),
            (TokenId::XautJetton, ChainId::Ton, Coin::Xaut, 6),
        ] {
            assert_eq!(token.chain(), chain);
            assert_eq!(token.coin(), coin);
            assert_eq!(token.decimals(), decimals);
        }
    }

    #[test]
    fn every_token_names_a_contract_on_its_own_chain() {
        for token in TokenId::ALL {
            let contract = token.contract();
            assert!(!contract.is_empty(), "{}", token.as_str());
            match token.chain() {
                ChainId::Ethereum
                | ChainId::Arbitrum
                | ChainId::Base
                | ChainId::Optimism
                | ChainId::Bsc
                | ChainId::Polygon
                | ChainId::Avalanche => {
                    assert!(contract.starts_with("0x"), "{contract}");
                    assert_eq!(contract.len(), 42, "{contract}");
                }
                ChainId::Tron => assert!(contract.starts_with('T'), "{contract}"),
                ChainId::Solana => assert!(!contract.starts_with("0x"), "{contract}"),
                ChainId::Ton => {
                    assert!(contract.starts_with("EQ"), "{contract}");
                    assert_eq!(contract.len(), 48, "{contract}");
                }
                other @ (ChainId::Bitcoin
                | ChainId::Litecoin
                | ChainId::Dogecoin
                | ChainId::BitcoinCash
                | ChainId::Zcash
                | ChainId::Xrp
                | ChainId::Monero) => {
                    panic!("{} has no tokens", other.as_str())
                }
            }
        }
    }

    /// Tether's omnichain `USDT0` and `XAUt0` on Polygon read as the coins they
    /// stand for, so a holder sees one USDT and one XAUT across networks.
    #[test]
    fn polygon_tokens_read_as_the_coins_they_stand_for() {
        for (token, coin) in [
            (TokenId::UsdtPolygon, Coin::Usdt),
            (TokenId::UsdcPolygon, Coin::Usdc),
            (TokenId::PyusdPolygon, Coin::Pyusd),
            (TokenId::XautPolygon, Coin::Xaut),
        ] {
            assert_eq!(token.chain(), ChainId::Polygon, "{}", token.as_str());
            assert_eq!(token.coin(), coin, "{}", token.as_str());
            assert_eq!(token.decimals(), 6, "{}", token.as_str());
        }
    }

    /// Every rollup pays gas in ether, so its coin trades as ETH.
    #[test]
    fn rollup_coins_are_ether() {
        for chain in [ChainId::Arbitrum, ChainId::Base, ChainId::Optimism] {
            assert_eq!(chain.coin(), Coin::Eth, "{}", chain.as_str());
            assert_eq!(chain.native_decimals(), 18, "{}", chain.as_str());
        }
        for (token, chain, coin, decimals) in [
            (TokenId::UsdtArbitrum, ChainId::Arbitrum, Coin::Usdt, 6),
            (TokenId::UsdcArbitrum, ChainId::Arbitrum, Coin::Usdc, 6),
            (TokenId::PyusdArbitrum, ChainId::Arbitrum, Coin::Pyusd, 6),
            (TokenId::XautArbitrum, ChainId::Arbitrum, Coin::Xaut, 6),
            (TokenId::UsdgArbitrum, ChainId::Arbitrum, Coin::Usdg, 6),
            (TokenId::UsdsArbitrum, ChainId::Arbitrum, Coin::Usds, 18),
            (TokenId::UsdcBase, ChainId::Base, Coin::Usdc, 6),
            (TokenId::UsdsBase, ChainId::Base, Coin::Usds, 18),
            (TokenId::OpOptimism, ChainId::Optimism, Coin::Op, 18),
            (TokenId::UsdtOptimism, ChainId::Optimism, Coin::Usdt, 6),
            (TokenId::UsdcOptimism, ChainId::Optimism, Coin::Usdc, 6),
            (TokenId::UsdsOptimism, ChainId::Optimism, Coin::Usds, 18),
        ] {
            assert_eq!(token.chain(), chain, "{}", token.as_str());
            assert_eq!(token.coin(), coin, "{}", token.as_str());
            assert_eq!(token.decimals(), decimals, "{}", token.as_str());
        }
    }

    /// AVAX lives on three chains, and the wallet holds it on the C-Chain.
    #[test]
    fn the_c_chain_coin_is_avax() {
        assert_eq!(ChainId::Avalanche.coin(), Coin::Avax);
        assert_eq!(ChainId::Avalanche.display_name(), "Avalanche C-Chain");
        assert_eq!(ChainId::Avalanche.native_decimals(), 18);
        for (token, coin, decimals) in [
            (TokenId::UsdtAvalanche, Coin::Usdt, 6),
            (TokenId::UsdcAvalanche, Coin::Usdc, 6),
            (TokenId::XautAvalanche, Coin::Xaut, 6),
            (TokenId::UsdsAvalanche, Coin::Usds, 18),
        ] {
            assert_eq!(token.chain(), ChainId::Avalanche, "{}", token.as_str());
            assert_eq!(token.coin(), coin, "{}", token.as_str());
            assert_eq!(token.decimals(), decimals, "{}", token.as_str());
        }
    }
}
