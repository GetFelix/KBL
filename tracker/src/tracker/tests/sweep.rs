//! A seeded sweep of the claim path under faults. Each seed drives imports,
//! claims, completions, dismissals and restarts against a Raft-style `Fake`
//! while the broker misbehaves, and checks the log after every step.
//!
//! `KBL_SWEEP_SEEDS=n` runs seeds 0..n (default 40), `KBL_SWEEP_SEED=s` replays
//! one seed. A failure names the seed. See docs/fault-sweep.md.

use kbl_core::work::CloseWhy;
use kbl_core::work_fold::{Closure, Stage, State};

use super::*;

const AGENTS: [&str; 3] = ["w1", "w2", "w3"];
const STEPS: usize = 70;

/// Cases of a bug the sweep found on main, which this change does not fix:
/// (seed, item, tracking KBL issue). A resume is served from the tracker's
/// memory, so a tracker that missed a failover's release hands out a claim
/// whose hold already ended. Nothing is written and the fold refuses the
/// agent's completion. The run expects exactly these items to be handed such
/// a claim, so a fix, or a change that moves the failure, fails the sweep
/// until the list is updated.
const PINNED_RESUMED: &[(u64, &str, u32)] = &[
    (8, "i4", 311),
    (15, "i5", 311),
    (25, "i1", 311),
    (27, "i5", 311),
    (28, "i0", 311),
    (42, "i1", 311),
    (43, "i0", 311),
    (49, "i3", 311),
    (53, "i7", 311),
    (55, "i0", 311),
    (57, "i7", 311),
    (63, "i0", 311),
    (64, "i7", 311),
    (65, "i1", 311),
    (68, "i2", 311),
    (73, "i11", 311),
    (77, "i3", 311),
    (79, "i3", 311),
    (82, "i8", 311),
    (84, "i7", 311),
    (94, "i6", 311),
    (99, "i0", 311),
    (99, "i5", 311),
    (105, "i0", 311),
    (105, "i2", 311),
    (107, "i0", 311),
    (110, "i1", 311),
    (113, "i10", 311),
    (115, "i4", 311),
    (122, "i6", 311),
    (123, "i1", 311),
    (124, "i1", 311),
    (131, "i0", 311),
    (139, "i4", 311),
    (143, "i1", 311),
    (143, "i3", 311),
    (144, "i2", 311),
    (146, "i2", 311),
    (150, "i0", 311),
    (151, "i1", 311),
    (162, "i13", 311),
    (163, "i2", 311),
    (168, "i2", 311),
    (181, "i3", 311),
    (189, "i1", 311),
    (190, "i7", 311),
    (201, "i1", 311),
    (206, "i4", 311),
    (213, "i10", 311),
    (243, "i6", 311),
    (245, "i0", 311),
    (251, "i5", 311),
    (259, "i3", 311),
    (262, "i2", 311),
    (274, "i1", 311),
    (284, "i4", 311),
    (288, "i11", 311),
    (300, "i1", 311),
    (302, "i4", 311),
    (317, "i2", 311),
    (317, "i6", 311),
    (338, "i1", 311),
    (344, "i1", 311),
    (347, "i0", 311),
    (357, "i1", 311),
    (357, "i3", 311),
    (366, "i1", 311),
    (368, "i6", 311),
    (373, "i3", 311),
    (375, "i1", 311),
    (376, "i2", 311),
    (389, "i2", 311),
    (389, "i3", 311),
    (390, "i1", 311),
    (390, "i12", 311),
    (390, "i5", 311),
    (395, "i5", 311),
    (396, "i1", 311),
    (403, "i0", 311),
    (411, "i3", 311),
    (413, "i1", 311),
    (417, "i6", 311),
    (426, "i4", 311),
    (428, "i1", 311),
    (432, "i5", 311),
    (434, "i1", 311),
    (434, "i2", 311),
    (461, "i2", 311),
    (462, "i1", 311),
    (466, "i6", 311),
    (468, "i3", 311),
    (471, "i11", 311),
    (472, "i1", 311),
    (480, "i1", 311),
    (484, "i0", 311),
    (486, "i3", 311),
    (491, "i1", 311),
    (491, "i2", 311),
    (492, "i3", 311),
    (499, "i3", 311),
    (501, "i0", 311),
    (505, "i2", 311),
    (511, "i5", 311),
    (514, "i3", 311),
    (517, "i2", 311),
    (521, "i0", 311),
    (522, "i3", 311),
    (535, "i5", 311),
    (539, "i1", 311),
    (539, "i3", 311),
    (546, "i1", 311),
    (557, "i2", 311),
    (564, "i8", 311),
    (566, "i4", 311),
    (569, "i9", 311),
    (579, "i0", 311),
    (586, "i4", 311),
    (589, "i5", 311),
    (606, "i1", 311),
    (611, "i1", 311),
    (615, "i1", 311),
    (618, "i2", 311),
    (620, "i4", 311),
    (627, "i2", 311),
    (630, "i1", 311),
    (650, "i6", 311),
    (651, "i10", 311),
    (653, "i0", 311),
    (658, "i3", 311),
    (660, "i2", 311),
    (663, "i6", 311),
    (665, "i2", 311),
    (684, "i7", 311),
    (689, "i4", 311),
    (697, "i3", 311),
    (700, "i4", 311),
    (714, "i5", 311),
    (719, "i3", 311),
    (721, "i0", 311),
    (727, "i5", 311),
    (728, "i14", 311),
    (729, "i0", 311),
    (732, "i1", 311),
    (744, "i3", 311),
    (745, "i2", 311),
    (749, "i3", 311),
    (754, "i0", 311),
    (754, "i5", 311),
    (759, "i2", 311),
    (761, "i2", 311),
    (763, "i0", 311),
    (775, "i2", 311),
    (777, "i5", 311),
    (780, "i2", 311),
    (782, "i7", 311),
    (785, "i5", 311),
    (796, "i6", 311),
    (803, "i5", 311),
    (817, "i2", 311),
    (824, "i11", 311),
    (830, "i1", 311),
    (843, "i7", 311),
    (875, "i1", 311),
    (876, "i1", 311),
    (887, "i10", 311),
    (891, "i3", 311),
    (898, "i2", 311),
    (900, "i1", 311),
    (903, "i6", 311),
    (920, "i3", 311),
    (929, "i4", 311),
    (930, "i2", 311),
    (939, "i1", 311),
    (955, "i6", 311),
    (959, "i0", 311),
    (961, "i6", 311),
    (963, "i3", 311),
    (977, "i0", 311),
    (979, "i12", 311),
    (979, "i3", 311),
    (979, "i5", 311),
    (997, "i4", 311),
    (999, "i1", 311),
    (999, "i6", 311),
    (1002, "i5", 311),
    (1010, "i1", 311),
    (1013, "i0", 311),
    (1017, "i0", 311),
    (1018, "i3", 311),
    (1032, "i3", 311),
    (1039, "i8", 311),
    (1041, "i3", 311),
    (1047, "i1", 311),
    (1053, "i2", 311),
    (1054, "i2", 311),
    (1067, "i2", 311),
    (1070, "i4", 311),
    (1073, "i0", 311),
    (1074, "i4", 311),
    (1082, "i0", 311),
    (1082, "i5", 311),
    (1086, "i7", 311),
    (1091, "i7", 311),
    (1092, "i5", 311),
    (1109, "i12", 311),
    (1111, "i0", 311),
    (1117, "i3", 311),
    (1122, "i8", 311),
    (1125, "i7", 311),
    (1129, "i6", 311),
    (1131, "i0", 311),
    (1131, "i6", 311),
    (1135, "i2", 311),
    (1144, "i3", 311),
    (1154, "i3", 311),
    (1157, "i5", 311),
    (1168, "i0", 311),
    (1191, "i7", 311),
    (1193, "i2", 311),
    (1199, "i1", 311),
    (1215, "i3", 311),
    (1215, "i5", 311),
    (1225, "i4", 311),
    (1228, "i1", 311),
    (1229, "i5", 311),
    (1238, "i2", 311),
    (1240, "i4", 311),
    (1247, "i1", 311),
    (1251, "i0", 311),
    (1251, "i5", 311),
    (1256, "i12", 311),
    (1258, "i5", 311),
    (1259, "i1", 311),
    (1260, "i0", 311),
    (1271, "i2", 311),
    (1284, "i10", 311),
    (1289, "i5", 311),
    (1295, "i6", 311),
    (1297, "i5", 311),
    (1299, "i4", 311),
    (1300, "i6", 311),
    (1311, "i6", 311),
    (1322, "i3", 311),
    (1322, "i9", 311),
    (1324, "i2", 311),
    (1330, "i0", 311),
    (1333, "i2", 311),
    (1338, "i3", 311),
    (1346, "i11", 311),
    (1354, "i11", 311),
    (1357, "i6", 311),
    (1359, "i0", 311),
    (1372, "i1", 311),
    (1381, "i5", 311),
    (1388, "i0", 311),
    (1394, "i6", 311),
    (1398, "i10", 311),
    (1399, "i2", 311),
    (1407, "i5", 311),
    (1418, "i1", 311),
    (1429, "i3", 311),
    (1439, "i4", 311),
    (1442, "i5", 311),
    (1457, "i2", 311),
    (1459, "i3", 311),
    (1460, "i3", 311),
    (1464, "i2", 311),
    (1464, "i3", 311),
    (1470, "i3", 311),
    (1482, "i0", 311),
    (1484, "i1", 311),
    (1485, "i1", 311),
    (1490, "i3", 311),
    (1490, "i5", 311),
    (1499, "i1", 311),
    (1504, "i6", 311),
    (1516, "i4", 311),
    (1524, "i3", 311),
    (1524, "i5", 311),
    (1526, "i5", 311),
    (1544, "i1", 311),
    (1556, "i1", 311),
    (1563, "i10", 311),
    (1567, "i0", 311),
    (1567, "i1", 311),
    (1567, "i5", 311),
    (1571, "i3", 311),
    (1576, "i6", 311),
    (1582, "i5", 311),
    (1583, "i11", 311),
    (1594, "i7", 311),
    (1595, "i5", 311),
    (1596, "i1", 311),
    (1598, "i1", 311),
    (1607, "i5", 311),
    (1613, "i3", 311),
    (1616, "i3", 311),
    (1617, "i0", 311),
    (1627, "i4", 311),
    (1631, "i1", 311),
    (1638, "i5", 311),
    (1639, "i1", 311),
    (1657, "i0", 311),
    (1658, "i1", 311),
    (1660, "i4", 311),
    (1660, "i6", 311),
    (1671, "i6", 311),
    (1679, "i6", 311),
    (1683, "i4", 311),
    (1684, "i1", 311),
    (1692, "i1", 311),
    (1693, "i10", 311),
    (1697, "i0", 311),
    (1699, "i1", 311),
    (1701, "i0", 311),
    (1705, "i5", 311),
    (1708, "i5", 311),
    (1717, "i6", 311),
    (1722, "i1", 311),
    (1724, "i0", 311),
    (1730, "i11", 311),
    (1737, "i0", 311),
    (1739, "i9", 311),
    (1747, "i1", 311),
    (1752, "i4", 311),
    (1754, "i11", 311),
    (1757, "i4", 311),
    (1758, "i4", 311),
    (1765, "i10", 311),
    (1765, "i2", 311),
    (1766, "i0", 311),
    (1769, "i11", 311),
    (1769, "i5", 311),
    (1779, "i4", 311),
    (1781, "i1", 311),
    (1791, "i1", 311),
    (1792, "i4", 311),
    (1792, "i6", 311),
    (1796, "i6", 311),
    (1804, "i10", 311),
    (1811, "i3", 311),
    (1815, "i4", 311),
    (1819, "i2", 311),
    (1820, "i2", 311),
    (1830, "i7", 311),
    (1831, "i7", 311),
    (1833, "i6", 311),
    (1839, "i0", 311),
    (1843, "i1", 311),
    (1843, "i4", 311),
    (1850, "i4", 311),
    (1861, "i10", 311),
    (1861, "i9", 311),
    (1862, "i1", 311),
    (1868, "i0", 311),
    (1877, "i10", 311),
    (1878, "i1", 311),
    (1881, "i6", 311),
    (1892, "i6", 311),
    (1893, "i6", 311),
    (1912, "i2", 311),
    (1915, "i7", 311),
    (1927, "i3", 311),
    (1933, "i10", 311),
    (1947, "i3", 311),
    (1962, "i8", 311),
    (1965, "i4", 311),
    (1971, "i3", 311),
    (1973, "i11", 311),
    (1974, "i1", 311),
    (1987, "i6", 311),
    (1988, "i5", 311),
    (1993, "i3", 311),
    (2003, "i0", 311),
    (2004, "i5", 311),
    (2005, "i1", 311),
    (2008, "i5", 311),
    (2015, "i8", 311),
    (2024, "i10", 311),
    (2024, "i12", 311),
    (2024, "i4", 311),
    (2028, "i0", 311),
    (2028, "i1", 311),
    (2045, "i2", 311),
    (2047, "i3", 311),
    (2052, "i4", 311),
    (2061, "i0", 311),
    (2074, "i1", 311),
    (2076, "i8", 311),
    (2080, "i3", 311),
    (2080, "i9", 311),
    (2086, "i4", 311),
    (2092, "i2", 311),
    (2096, "i1", 311),
    (2097, "i0", 311),
    (2111, "i5", 311),
    (2123, "i0", 311),
    (2134, "i1", 311),
    (2141, "i6", 311),
    (2149, "i3", 311),
    (2151, "i0", 311),
    (2151, "i3", 311),
    (2156, "i3", 311),
    (2158, "i1", 311),
    (2159, "i7", 311),
    (2159, "i8", 311),
    (2176, "i4", 311),
    (2180, "i8", 311),
    (2192, "i5", 311),
    (2201, "i3", 311),
    (2205, "i8", 311),
    (2218, "i2", 311),
    (2227, "i2", 311),
    (2227, "i3", 311),
    (2229, "i2", 311),
    (2249, "i5", 311),
    (2262, "i3", 311),
    (2263, "i8", 311),
    (2266, "i4", 311),
    (2271, "i13", 311),
    (2274, "i1", 311),
    (2278, "i1", 311),
    (2290, "i4", 311),
    (2294, "i0", 311),
    (2306, "i5", 311),
    (2314, "i12", 311),
    (2318, "i1", 311),
    (2321, "i8", 311),
    (2323, "i8", 311),
    (2325, "i2", 311),
    (2335, "i4", 311),
    (2350, "i4", 311),
    (2354, "i2", 311),
    (2356, "i2", 311),
    (2360, "i10", 311),
    (2366, "i0", 311),
    (2366, "i4", 311),
    (2368, "i6", 311),
    (2379, "i1", 311),
    (2381, "i3", 311),
    (2390, "i7", 311),
    (2392, "i2", 311),
    (2393, "i2", 311),
    (2402, "i1", 311),
    (2415, "i5", 311),
    (2429, "i5", 311),
    (2433, "i3", 311),
    (2435, "i6", 311),
    (2436, "i2", 311),
    (2440, "i3", 311),
    (2461, "i1", 311),
    (2465, "i1", 311),
    (2476, "i1", 311),
    (2482, "i0", 311),
    (2483, "i3", 311),
    (2487, "i11", 311),
    (2491, "i1", 311),
    (2491, "i6", 311),
    (2493, "i1", 311),
    (2509, "i12", 311),
    (2510, "i0", 311),
    (2517, "i5", 311),
    (2520, "i0", 311),
    (2529, "i0", 311),
    (2539, "i0", 311),
    (2545, "i6", 311),
    (2553, "i3", 311),
    (2554, "i6", 311),
    (2556, "i2", 311),
    (2560, "i7", 311),
    (2569, "i10", 311),
    (2570, "i3", 311),
    (2575, "i0", 311),
    (2577, "i5", 311),
    (2578, "i0", 311),
    (2582, "i6", 311),
    (2590, "i5", 311),
    (2598, "i1", 311),
    (2600, "i4", 311),
    (2606, "i1", 311),
    (2608, "i2", 311),
    (2610, "i0", 311),
    (2624, "i8", 311),
    (2629, "i1", 311),
    (2630, "i0", 311),
    (2632, "i4", 311),
    (2636, "i1", 311),
    (2637, "i1", 311),
    (2644, "i6", 311),
    (2648, "i6", 311),
    (2665, "i2", 311),
    (2676, "i8", 311),
    (2676, "i9", 311),
    (2677, "i4", 311),
    (2678, "i7", 311),
    (2690, "i5", 311),
    (2695, "i3", 311),
    (2700, "i10", 311),
    (2704, "i8", 311),
    (2707, "i3", 311),
    (2707, "i6", 311),
    (2709, "i1", 311),
    (2710, "i10", 311),
    (2714, "i0", 311),
    (2721, "i3", 311),
    (2723, "i3", 311),
    (2726, "i12", 311),
    (2729, "i8", 311),
    (2732, "i1", 311),
    (2734, "i1", 311),
    (2734, "i2", 311),
    (2737, "i2", 311),
    (2742, "i1", 311),
    (2742, "i11", 311),
    (2751, "i0", 311),
    (2754, "i4", 311),
    (2768, "i6", 311),
    (2770, "i0", 311),
    (2781, "i3", 311),
    (2791, "i6", 311),
    (2807, "i0", 311),
    (2810, "i6", 311),
    (2813, "i2", 311),
    (2814, "i1", 311),
    (2818, "i1", 311),
    (2819, "i2", 311),
    (2820, "i0", 311),
    (2830, "i5", 311),
    (2844, "i10", 311),
    (2848, "i5", 311),
    (2853, "i10", 311),
    (2856, "i3", 311),
    (2856, "i7", 311),
    (2864, "i10", 311),
    (2864, "i5", 311),
    (2866, "i0", 311),
    (2871, "i11", 311),
    (2872, "i1", 311),
    (2878, "i11", 311),
    (2885, "i3", 311),
    (2901, "i9", 311),
    (2909, "i0", 311),
    (2919, "i5", 311),
    (2920, "i3", 311),
    (2933, "i5", 311),
    (2934, "i5", 311),
    (2939, "i1", 311),
    (2950, "i3", 311),
    (2951, "i4", 311),
    (2966, "i0", 311),
    (2973, "i1", 311),
    (2975, "i7", 311),
    (2981, "i10", 311),
    (2983, "i1", 311),
    (3006, "i5", 311),
    (3006, "i7", 311),
    (3014, "i4", 311),
    (3017, "i2", 311),
    (3019, "i2", 311),
    (3026, "i2", 311),
    (3040, "i3", 311),
    (3042, "i1", 311),
    (3045, "i3", 311),
    (3047, "i7", 311),
    (3053, "i1", 311),
    (3057, "i12", 311),
    (3058, "i5", 311),
    (3059, "i3", 311),
    (3061, "i1", 311),
    (3065, "i3", 311),
    (3070, "i0", 311),
    (3081, "i3", 311),
    (3092, "i4", 311),
    (3099, "i1", 311),
    (3106, "i1", 311),
    (3107, "i10", 311),
    (3113, "i1", 311),
    (3117, "i1", 311),
    (3119, "i1", 311),
    (3124, "i2", 311),
    (3126, "i0", 311),
    (3140, "i3", 311),
    (3141, "i1", 311),
    (3147, "i8", 311),
    (3152, "i7", 311),
    (3165, "i7", 311),
    (3170, "i3", 311),
    (3174, "i5", 311),
    (3187, "i5", 311),
    (3188, "i9", 311),
    (3189, "i0", 311),
    (3190, "i6", 311),
    (3201, "i2", 311),
    (3203, "i7", 311),
    (3206, "i2", 311),
    (3207, "i5", 311),
    (3208, "i4", 311),
    (3212, "i2", 311),
    (3212, "i7", 311),
    (3215, "i0", 311),
    (3226, "i3", 311),
    (3227, "i1", 311),
    (3228, "i1", 311),
    (3229, "i3", 311),
    (3231, "i1", 311),
    (3242, "i4", 311),
    (3248, "i3", 311),
    (3249, "i5", 311),
    (3256, "i10", 311),
    (3259, "i1", 311),
    (3265, "i5", 311),
    (3269, "i11", 311),
    (3272, "i2", 311),
    (3285, "i0", 311),
    (3289, "i0", 311),
    (3290, "i8", 311),
    (3304, "i1", 311),
    (3315, "i12", 311),
    (3319, "i4", 311),
    (3327, "i6", 311),
    (3327, "i7", 311),
    (3333, "i9", 311),
    (3334, "i7", 311),
    (3338, "i5", 311),
    (3348, "i4", 311),
    (3355, "i1", 311),
    (3365, "i2", 311),
    (3369, "i7", 311),
    (3370, "i4", 311),
    (3374, "i3", 311),
    (3378, "i3", 311),
    (3381, "i0", 311),
    (3391, "i7", 311),
    (3394, "i6", 311),
    (3406, "i12", 311),
    (3415, "i0", 311),
    (3418, "i0", 311),
    (3419, "i3", 311),
    (3429, "i3", 311),
    (3431, "i6", 311),
    (3433, "i3", 311),
    (3442, "i10", 311),
    (3445, "i1", 311),
    (3450, "i0", 311),
    (3451, "i5", 311),
    (3462, "i11", 311),
    (3473, "i0", 311),
    (3473, "i3", 311),
    (3475, "i3", 311),
    (3477, "i3", 311),
    (3480, "i0", 311),
    (3488, "i1", 311),
    (3491, "i2", 311),
    (3493, "i2", 311),
    (3496, "i10", 311),
    (3504, "i2", 311),
    (3504, "i6", 311),
    (3505, "i7", 311),
    (3516, "i4", 311),
    (3517, "i4", 311),
    (3528, "i0", 311),
    (3529, "i0", 311),
    (3531, "i0", 311),
    (3546, "i6", 311),
    (3550, "i4", 311),
    (3557, "i1", 311),
    (3565, "i5", 311),
    (3567, "i6", 311),
    (3582, "i0", 311),
    (3582, "i1", 311),
    (3583, "i0", 311),
    (3594, "i1", 311),
    (3598, "i10", 311),
    (3599, "i2", 311),
    (3599, "i7", 311),
    (3603, "i1", 311),
    (3609, "i9", 311),
    (3618, "i9", 311),
    (3625, "i1", 311),
    (3627, "i4", 311),
    (3630, "i7", 311),
    (3638, "i4", 311),
    (3643, "i1", 311),
    (3644, "i0", 311),
    (3672, "i4", 311),
    (3673, "i0", 311),
    (3674, "i1", 311),
    (3674, "i7", 311),
    (3675, "i9", 311),
    (3679, "i1", 311),
    (3680, "i0", 311),
    (3685, "i3", 311),
    (3699, "i4", 311),
    (3703, "i5", 311),
    (3719, "i7", 311),
    (3721, "i3", 311),
    (3727, "i2", 311),
    (3729, "i6", 311),
    (3736, "i0", 311),
    (3739, "i4", 311),
    (3742, "i11", 311),
    (3752, "i5", 311),
    (3757, "i10", 311),
    (3762, "i3", 311),
    (3765, "i0", 311),
    (3766, "i0", 311),
    (3769, "i2", 311),
    (3771, "i2", 311),
    (3773, "i4", 311),
    (3775, "i4", 311),
    (3778, "i4", 311),
    (3779, "i6", 311),
    (3784, "i1", 311),
    (3784, "i5", 311),
    (3787, "i1", 311),
    (3792, "i3", 311),
    (3794, "i10", 311),
    (3803, "i1", 311),
    (3808, "i2", 311),
    (3808, "i3", 311),
    (3815, "i1", 311),
    (3824, "i0", 311),
    (3827, "i1", 311),
    (3827, "i3", 311),
    (3829, "i4", 311),
    (3832, "i4", 311),
    (3835, "i1", 311),
    (3844, "i7", 311),
    (3847, "i3", 311),
    (3847, "i6", 311),
    (3848, "i1", 311),
    (3850, "i0", 311),
    (3853, "i1", 311),
    (3866, "i3", 311),
    (3871, "i0", 311),
    (3873, "i3", 311),
    (3875, "i0", 311),
    (3875, "i2", 311),
    (3885, "i1", 311),
    (3887, "i4", 311),
    (3888, "i0", 311),
    (3888, "i6", 311),
    (3890, "i0", 311),
    (3897, "i1", 311),
    (3901, "i0", 311),
    (3905, "i0", 311),
    (3911, "i0", 311),
    (3915, "i1", 311),
    (3915, "i10", 311),
    (3929, "i10", 311),
    (3929, "i9", 311),
    (3931, "i4", 311),
    (3937, "i4", 311),
    (3939, "i7", 311),
    (3948, "i2", 311),
    (3951, "i4", 311),
    (3955, "i5", 311),
    (3960, "i7", 311),
    (3970, "i4", 311),
    (3973, "i0", 311),
    (3973, "i5", 311),
    (3976, "i2", 311),
    (3998, "i3", 311),
    (4000, "i4", 311),
    (4008, "i5", 311),
    (4008, "i9", 311),
    (4009, "i3", 311),
    (4009, "i7", 311),
    (4022, "i2", 311),
    (4035, "i11", 311),
    (4036, "i10", 311),
    (4037, "i5", 311),
    (4038, "i6", 311),
    (4039, "i2", 311),
    (4040, "i3", 311),
    (4042, "i0", 311),
    (4043, "i1", 311),
    (4051, "i3", 311),
    (4051, "i7", 311),
    (4052, "i10", 311),
    (4053, "i1", 311),
    (4055, "i10", 311),
    (4066, "i0", 311),
    (4074, "i8", 311),
    (4077, "i3", 311),
    (4078, "i4", 311),
    (4079, "i3", 311),
    (4079, "i4", 311),
    (4085, "i2", 311),
    (4090, "i2", 311),
    (4093, "i2", 311),
    (4094, "i4", 311),
    (4097, "i0", 311),
    (4100, "i3", 311),
    (4106, "i6", 311),
    (4111, "i3", 311),
    (4111, "i4", 311),
    (4116, "i4", 311),
    (4123, "i2", 311),
    (4128, "i7", 311),
    (4128, "i8", 311),
    (4132, "i5", 311),
    (4143, "i0", 311),
    (4155, "i4", 311),
    (4156, "i0", 311),
    (4156, "i5", 311),
    (4160, "i4", 311),
    (4160, "i6", 311),
    (4180, "i0", 311),
    (4180, "i9", 311),
    (4188, "i3", 311),
    (4209, "i4", 311),
    (4226, "i10", 311),
    (4226, "i11", 311),
    (4226, "i2", 311),
    (4230, "i7", 311),
    (4250, "i11", 311),
    (4259, "i9", 311),
    (4262, "i4", 311),
    (4266, "i5", 311),
    (4271, "i0", 311),
    (4275, "i4", 311),
    (4294, "i0", 311),
    (4299, "i8", 311),
    (4312, "i3", 311),
    (4318, "i0", 311),
    (4325, "i12", 311),
    (4326, "i2", 311),
    (4334, "i3", 311),
    (4344, "i3", 311),
    (4345, "i2", 311),
    (4347, "i6", 311),
    (4351, "i7", 311),
    (4356, "i4", 311),
    (4365, "i2", 311),
    (4369, "i4", 311),
    (4374, "i6", 311),
    (4401, "i0", 311),
    (4404, "i0", 311),
    (4405, "i5", 311),
    (4411, "i6", 311),
    (4413, "i8", 311),
    (4427, "i7", 311),
    (4436, "i1", 311),
    (4444, "i1", 311),
    (4445, "i9", 311),
    (4454, "i0", 311),
    (4460, "i0", 311),
    (4460, "i2", 311),
    (4462, "i3", 311),
    (4462, "i7", 311),
    (4468, "i2", 311),
    (4471, "i3", 311),
    (4480, "i6", 311),
    (4486, "i0", 311),
    (4487, "i10", 311),
    (4491, "i0", 311),
    (4491, "i2", 311),
    (4494, "i4", 311),
    (4510, "i3", 311),
    (4517, "i5", 311),
    (4520, "i10", 311),
    (4523, "i1", 311),
    (4526, "i8", 311),
    (4540, "i0", 311),
    (4549, "i0", 311),
    (4552, "i1", 311),
    (4552, "i4", 311),
    (4554, "i6", 311),
    (4555, "i5", 311),
    (4556, "i11", 311),
    (4556, "i5", 311),
    (4559, "i7", 311),
    (4564, "i1", 311),
    (4568, "i0", 311),
    (4581, "i4", 311),
    (4585, "i0", 311),
    (4595, "i6", 311),
    (4596, "i0", 311),
    (4599, "i3", 311),
    (4612, "i1", 311),
    (4614, "i4", 311),
    (4615, "i0", 311),
    (4622, "i10", 311),
    (4624, "i3", 311),
    (4628, "i8", 311),
    (4629, "i8", 311),
    (4632, "i2", 311),
    (4633, "i3", 311),
    (4644, "i3", 311),
    (4649, "i3", 311),
    (4663, "i8", 311),
    (4667, "i1", 311),
    (4670, "i3", 311),
    (4683, "i0", 311),
    (4683, "i4", 311),
    (4683, "i9", 311),
    (4685, "i5", 311),
    (4686, "i6", 311),
    (4688, "i9", 311),
    (4689, "i10", 311),
    (4697, "i1", 311),
    (4700, "i3", 311),
    (4703, "i0", 311),
    (4707, "i3", 311),
    (4710, "i4", 311),
    (4720, "i4", 311),
    (4733, "i2", 311),
    (4733, "i4", 311),
    (4738, "i5", 311),
    (4738, "i8", 311),
    (4739, "i6", 311),
    (4741, "i3", 311),
    (4770, "i0", 311),
    (4771, "i10", 311),
    (4783, "i2", 311),
    (4789, "i3", 311),
    (4803, "i3", 311),
    (4809, "i0", 311),
    (4813, "i6", 311),
    (4818, "i3", 311),
    (4819, "i0", 311),
    (4822, "i4", 311),
    (4830, "i4", 311),
    (4831, "i10", 311),
    (4838, "i9", 311),
    (4840, "i6", 311),
    (4852, "i5", 311),
    (4860, "i6", 311),
    (4862, "i2", 311),
    (4867, "i9", 311),
    (4869, "i8", 311),
    (4881, "i2", 311),
    (4884, "i4", 311),
    (4897, "i0", 311),
    (4900, "i10", 311),
    (4900, "i4", 311),
    (4905, "i0", 311),
    (4919, "i3", 311),
    (4923, "i4", 311),
    (4928, "i10", 311),
    (4932, "i15", 311),
    (4937, "i2", 311),
    (4944, "i10", 311),
    (4953, "i1", 311),
    (4982, "i2", 311),
    (4983, "i3", 311),
    (4989, "i0", 311),
    (4989, "i5", 311),
    (4996, "i1", 311),
];

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// Prints the seed when a check inside the run panics.
struct Replay(u64);

impl Drop for Replay {
    fn drop(&mut self) {
        if std::thread::panicking() {
            eprintln!(
                "fault sweep failed on seed {}; replay: KBL_SWEEP_SEED={} cargo test -p kbl-tracker sweep",
                self.0, self.0
            );
        }
    }
}

/// Replaying one seed ignores the pins and fails on what it finds.
fn pins_apply() -> bool {
    std::env::var("KBL_SWEEP_SEED").is_err()
}

fn judge(seed: u64, m: &Model) {
    if !pins_apply() {
        return;
    }
    let mut pinned: Vec<&str> = PINNED_RESUMED
        .iter()
        .filter(|(s, ..)| *s == seed)
        .map(|(_, id, _)| *id)
        .collect();
    let mut seen: Vec<&str> = m.stale_resumes.iter().map(String::as_str).collect();
    pinned.sort();
    pinned.dedup();
    seen.sort();
    seen.dedup();
    assert_eq!(
        seen, pinned,
        "stale resumed claims differ from the pinned ones; update PINNED_RESUMED"
    );
}

fn seeds() -> std::ops::Range<u64> {
    let var = |k: &str| std::env::var(k).ok().and_then(|v| v.parse::<u64>().ok());
    if let Some(one) = var("KBL_SWEEP_SEED") {
        return one..one + 1;
    }
    0..var("KBL_SWEEP_SEEDS").unwrap_or(40)
}

/// The log folded from nothing, and the offsets of the claims whose hold has
/// since ended.
struct Replayed {
    state: State,
    ended: HashSet<u64>,
}

/// The stream as the log's readers see it, folded from nothing. Panics on a
/// record the fold refuses, and on a claim of an item that is already held.
fn replay(fake: &Fake) -> Replayed {
    let records = fake
        .net
        .lock()
        .unwrap()
        .streams
        .get(WORK_EVENTS)
        .cloned()
        .unwrap_or_default();
    let mut state = State::default();
    let mut dones: HashSet<(String, u32)> = HashSet::new();
    let mut open: HashMap<ItemId, u64> = HashMap::new();
    let mut ended = HashSet::new();
    for (offset, payload) in records {
        let logged = WorkLogged::decode(&payload).unwrap();
        match &logged.record {
            WorkRecord::Claimed { item, .. } => {
                let held = state.items.get(item).and_then(|i| i.hold.as_ref());
                assert!(held.is_none(), "{item} claimed at {offset} while held");
            }
            WorkRecord::Done { item, attempt, .. } => {
                assert!(
                    dones.insert((item.to_string(), *attempt)),
                    "{item} attempt {attempt} done twice"
                );
            }
            _ => {}
        }
        state
            .try_apply(offset, &logged)
            .unwrap_or_else(|why| panic!("the fold refused the record at {offset}: {why:?}"));
        if let Some(id) = logged.record.item() {
            if matches!(logged.record, WorkRecord::Claimed { .. }) {
                open.insert(id.clone(), offset);
            } else if state.items.get(id).is_some_and(|i| i.hold.is_none()) {
                ended.extend(open.remove(id));
            }
        }
    }
    Replayed { state, ended }
}

/// The tracker agents talk to, and the one it replaced, which has not noticed
/// yet. Until its lease check or a refused write fences it, the old tracker
/// still serves from a state another writer has moved past.
struct Trackers {
    cur: Tracker<Fake>,
    old: Option<Tracker<Fake>>,
}

impl Trackers {
    fn serving(&self, old: bool) -> &Tracker<Fake> {
        match &self.old {
            Some(t) if old => t,
            _ => &self.cur,
        }
    }
}

#[derive(Default)]
struct Model {
    next_item: u32,
    /// What each agent believes it holds: item and attempt.
    beliefs: HashMap<&'static str, String>,
    /// Who was handed each claim, by the offset of its `claimed` record.
    handed: HashMap<u64, &'static str>,
    /// Items whose import was acknowledged, and whose accept was.
    imported: HashSet<String>,
    accepted: HashSet<String>,
    dismissed: HashSet<String>,
    done: HashSet<String>,
    /// Records another broker's writer committed.
    foreign: u32,
    /// Who was handed each attempt of each item, and at which claim. Kept apart
    /// from the log so a claim that lands twice cannot hide behind the fold.
    started: HashMap<(String, u32), (String, u64)>,
    /// Items handed a resumed claim whose hold had already ended.
    stale_resumes: Vec<String>,
}

impl Model {
    /// An attempt goes to a second agent only after the first one's hold ended.
    fn start(&mut self, ended: &HashSet<u64>, id: &str, attempt: u32, who: &str, at: u64) {
        let prior = self
            .started
            .insert((id.to_owned(), attempt), (who.to_owned(), at));
        if let Some((other, was)) = prior
            && other != who
        {
            assert!(
                was != at && ended.contains(&was),
                "{id} attempt {attempt} went to {other} (claim at {was}) and to {who} (claim at {at})"
            );
        }
    }

    fn check(&self, fake: &Fake) {
        let state = replay(fake).state;
        for id in &self.dismissed {
            let it = &state.items[&item(id)];
            assert_eq!(it.closed, Some(Closure::Dismissed), "{id} was reopened");
            assert!(it.hold.is_none(), "{id} is held after its dismissal");
        }
        for id in &self.imported {
            assert!(state.items.contains_key(&item(id)), "{id} was lost");
        }
    }
}

async fn restart(fake: &Fake) -> Tracker<Fake> {
    start(fake).await
}

fn ready_streams() -> [String; 4] {
    [0u8, 1, 2, 3].map(ready_stream)
}

async fn seed_item(t: &Tracker<Fake>, m: &mut Model) {
    let id = format!("i{}", m.next_item);
    m.next_item += 1;
    let put = |record| t.person_record(key("gab"), record);
    if put(imported(&id)).await.is_err() {
        return;
    }
    m.imported.insert(id.clone());
    let accepted = WorkRecord::Accepted {
        item: item(&id),
        tier: Some(2),
    };
    if put(accepted).await.is_ok() {
        m.accepted.insert(id);
    }
}

async fn claim_step(t: &Tracker<Fake>, fake: &Fake, m: &mut Model, who: &'static str) {
    let Ok(reply) = t.next(&agent(who), Duration::ZERO).await else {
        return;
    };
    let Some(id) = claimed_id(&reply) else {
        return;
    };
    let attempt = reply["attempt"].as_u64().unwrap() as u32;
    let at = reply["offset"].as_u64().expect("a claim names its record");
    let resumed = reply["resumed"] == true;
    let Replayed { state, ended } = replay(fake);
    if resumed && ended.contains(&at) {
        assert!(
            pins_apply(),
            "{who} was resumed {id}'s claim at {at}, whose hold had ended"
        );
        m.stale_resumes.push(id);
        return;
    }
    let hold = state.items[&item(&id)]
        .hold
        .clone()
        .expect("a claim with no hold");
    assert_eq!(
        (hold.by.as_str(), hold.attempt),
        (who, attempt),
        "{who} was told it holds {id} attempt {attempt}"
    );
    // A resume may be of a claim whose first reply was lost; a fresh claim is
    // only ever handed out once.
    match m.handed.insert(at, who) {
        Some(other) if resumed => assert_eq!(other, who, "{who} resumed {other}'s claim on {id}"),
        Some(other) => panic!("{id} claim at {at} went to {other} and to {who}"),
        None => {}
    }
    m.start(&ended, &id, attempt, who, at);
    m.beliefs.insert(who, id);
}

async fn finish_step(t: &Tracker<Fake>, m: &mut Model, rng: &mut Rng, who: &'static str) {
    let Some(id) = m.beliefs.get(who).cloned() else {
        return;
    };
    let a = agent(who);
    let sent = match rng.below(3) {
        0 => {
            let r = t.complete(&a, &item(&id), None, Vec::new()).await;
            r.map(|_| true)
        }
        1 => t
            .release(&a, &item(&id), None, None, Vec::new())
            .await
            .map(|_| false),
        _ => t
            .give_up(&a, &item(&id), None, "stuck".into(), Vec::new())
            .await
            .map(|_| false),
    };
    if let Ok(done) = sent {
        m.beliefs.remove(who);
        if done {
            m.done.insert(id);
        }
    }
}

async fn dismiss_step(t: &Tracker<Fake>, m: &mut Model, rng: &mut Rng) {
    let mut ids: Vec<_> = m.imported.iter().cloned().collect();
    ids.sort();
    if ids.is_empty() {
        return;
    }
    let id = ids[rng.below(ids.len())].clone();
    let closed = WorkRecord::Closed {
        item: item(&id),
        why: CloseWhy::Dismissed,
    };
    if t.person_record(key("gab"), closed).await.is_ok() {
        m.dismissed.insert(id);
    }
}

/// Another broker's writer lands a record at the tail: a failover. Half the
/// time it is a claim or a release by another worker, which the fold accepts,
/// so a tracker that writes without its tail check lands a second claim.
async fn foreign_write(fake: &Fake, m: &mut Model, rng: &mut Rng) {
    m.foreign += 1;
    let Replayed { state, ended } = replay(fake);
    let at = fake.net.lock().unwrap().next_offset(WORK_EVENTS);
    let ids: Vec<ItemId> = state.items.keys().cloned().collect();
    let kind = rng.below(4);
    let first = rng.below(ids.len().max(1));
    let mut theirs = None;
    for k in 0..ids.len() {
        let id = &ids[(first + k) % ids.len()];
        let it = &state.items[id];
        if it.closed.is_some() {
            continue;
        }
        let (by, record) = match (kind, &it.hold) {
            (0, None) => (
                key("ghost"),
                WorkRecord::Claimed {
                    item: id.clone(),
                    attempt: it.next_attempt(),
                    deadline_ms: now_ms() + 60_000,
                },
            ),
            (1, Some(h)) => (
                h.by.clone(),
                WorkRecord::Released {
                    item: id.clone(),
                    attempt: h.attempt,
                    reason: None,
                    cost_micros: 0,
                    usage: Vec::new(),
                    floor_micros: 0,
                },
            ),
            _ => continue,
        };
        let logged = WorkLogged::new(0, by, Actor::Agent, record);
        if state.clone().try_apply(at, &logged).is_ok() {
            theirs = Some(logged);
            break;
        }
    }
    let theirs = theirs.unwrap_or_else(|| {
        WorkLogged::new(
            0,
            key("gab"),
            Actor::Person,
            imported(&format!("f{}", m.foreign)),
        )
    });
    let offset = fake.publish(WORK_EVENTS, theirs.encode()).await.unwrap();
    if let WorkRecord::Claimed { item, attempt, .. } = &theirs.record {
        m.start(&ended, item.as_str(), *attempt, "ghost", offset);
    }
}

/// One step: maybe a fault, then an operation, then the checks.
async fn step(fake: &Fake, t: &mut Trackers, m: &mut Model, rng: &mut Rng) {
    let mut crash_after = false;
    match rng.below(100) {
        0..=7 => {
            // The tracker dies after some number of its writes.
            fake.fail_publishes_to_after(WORK_EVENTS, rng.below(3) as u32);
            crash_after = true;
        }
        8..=12 => {
            // The old tracker stays up until something fences it.
            let new = restart(fake).await;
            t.old = Some(std::mem::replace(&mut t.cur, new));
        }
        13..=17 => fake.lose_next_publish_reply(WORK_EVENTS),
        18..=22 => foreign_write(fake, m, rng).await,
        23..=30 => {
            // Felix hands a record out again: a lapsed lease, or a repeat of an acked one.
            let streams = ready_streams();
            let stream = &streams[rng.below(4)];
            let len = fake.ready_count(stream);
            if len > 0 {
                fake.lapse(stream, rng.below(len) as u64);
            }
        }
        31..=34 => fake.fail_polls(Some("work.ready")),
        35..=40 => {
            pass(&t.cur, Duration::from_secs(10 + rng.below(90) as u64)).await;
            t.cur.sweep().await;
        }
        _ => {}
    }
    let who = AGENTS[rng.below(AGENTS.len())];
    let serving = t.serving(rng.below(100) < 40);
    match rng.below(10) {
        0..=1 => seed_item(serving, m).await,
        2..=5 => claim_step(serving, fake, m, who).await,
        6..=8 => finish_step(serving, m, rng, who).await,
        _ => dismiss_step(serving, m, rng).await,
    }
    fake.fail_polls(None);
    if crash_after {
        fake.lift_publish_budget();
        t.cur = restart(fake).await;
    }
    m.check(fake);
}

/// Faults stop, a tracker restarts, and the agents drain the queue.
async fn heal(fake: &Fake, m: &mut Model) {
    fake.lift_publish_budget();
    fake.fail_polls(None);
    let t = restart(fake).await;
    // A person retries an accept that was not acknowledged.
    let mut imported: Vec<_> = m.imported.iter().cloned().collect();
    imported.sort();
    for id in imported {
        if m.accepted.contains(&id) || m.dismissed.contains(&id) {
            continue;
        }
        let accepted = WorkRecord::Accepted {
            item: item(&id),
            tier: Some(2),
        };
        if t.person_record(key("gab"), accepted).await.is_ok() {
            m.accepted.insert(id);
        }
    }
    // A poll may only drop a stale record, and a ready record Felix lost track
    // of is published again once it has stood a few visibility periods, so
    // the quiet has to outlast that.
    let mut quiet = 0u32;
    for _ in 0..300 {
        pass(&t, VIS + Duration::from_secs(5)).await;
        // Felix's visibility timeout passes for every delivery nobody settled.
        fake.lapse_all();
        t.sweep().await;
        let mut idle = true;
        for who in AGENTS {
            claim_step(&t, fake, m, who).await;
            if let Some(id) = m.beliefs.get(who).cloned() {
                idle = false;
                if t.complete(&agent(who), &item(&id), None, Vec::new())
                    .await
                    .is_ok()
                {
                    m.beliefs.remove(who);
                    m.done.insert(id);
                }
            }
        }
        m.check(fake);
        quiet = if idle { quiet + 1 } else { 0 };
        if quiet == RECORD_LAPSE_PERIODS + 2 {
            break;
        }
    }
    let state = replay(fake).state;
    let log = fake.log();
    for id in &m.accepted {
        if m.dismissed.contains(id) {
            continue;
        }
        let finished = log
            .iter()
            .any(|l| matches!(&l.record, WorkRecord::Done { item: i, .. } if i == &item(id)));
        // A person may have dismissed an item whose reply was lost.
        let closed = state.items[&item(id)].closed.is_some();
        // Out of claims or given up on twice, an item waits for a person.
        let waits = state.stage(&item(id)) == Some(Stage::NeedsYou);
        assert!(
            finished || closed || waits,
            "{id} was accepted and never worked (stage {:?})",
            state.stage(&item(id))
        );
    }
}

#[tokio::test(start_paused = true)]
async fn the_claim_path_holds_its_invariants_under_seeded_faults() {
    for seed in seeds() {
        let _replay = Replay(seed);
        let mut rng = Rng(seed);
        let fake = Fake::default();
        // Half the seeds run on a Raft shard, where a refusal burns an offset.
        if seed % 2 == 0 {
            fake.raft_blanks();
        }
        let mut t = Trackers {
            cur: start(&fake).await,
            old: None,
        };
        let mut m = Model::default();
        for _ in 0..STEPS {
            step(&fake, &mut t, &mut m, &mut rng).await;
        }
        heal(&fake, &mut m).await;
        judge(seed, &m);
    }
}
