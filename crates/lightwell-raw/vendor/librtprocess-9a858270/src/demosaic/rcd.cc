/*
 *  This file is part of RawTherapee.
 *
 *  Copyright (c) 2017-2018 Luis Sanz Rodriguez (luis.sanz.rodriguez(at)gmail(dot)com) and Ingo Weyrich (heckflosse67@gmx.de)
 *
 *  RawTherapee is free software: you can redistribute it and/or modify
 *  it under the terms of the GNU General Public License as published by
 *  the Free Software Foundation, either version 3 of the License, or
 *  (at your option) any later version.
 *
 *  RawTherapee is distributed in the hope that it will be useful,
 *  but WITHOUT ANY WARRANTY; without even the implied warranty of
 *  MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
 *  GNU General Public License for more details.
 *
 *  You should have received a copy of the GNU General Public License
 *  along with RawTherapee.  If not, see <http://www.gnu.org/licenses/>.
 */
#include <atomic>
#include <cmath>
#include <cstdlib>
#include <functional>
#include <memory>
#include <stdexcept>

#include "bayerhelper.h"
#include "librtprocess.h"
#include "opthelper.h"
#include "rt_math.h"
#include "StopWatch.h"

using namespace librtprocess;

namespace {
struct RcdWorkerCall {
    std::function<void(size_t)> run;
    std::atomic<rpError> *error;
};

extern "C" void run_rcd_worker(void *context, size_t job) noexcept
{
    auto *call = static_cast<RcdWorkerCall *>(context);
    try {
        call->run(job);
    } catch (const std::bad_alloc &) {
        rpError expected = RP_NO_ERROR;
        call->error->compare_exchange_strong(expected, RP_MEMORY_ERROR);
    } catch (...) {
        rpError expected = RP_NO_ERROR;
        call->error->compare_exchange_strong(expected, RP_WORKER_ERROR);
    }
}
}

/*
* RATIO CORRECTED DEMOSAICING
* Luis Sanz Rodriguez (luis.sanz.rodriguez(at)gmail(dot)com)
*
* Release 2.3 @ 171125
*
* Original code from https://github.com/LuisSR/RCD-Demosaicing
* Licensed under the GNU GPL version 3
*/

// Tiled version by Ingo Weyrich (heckflosse67@gmx.de)
// Luis Sanz Rodriguez significantly optimised the v 2.3 code and simplified the directional
// coefficients in an exact, shorter and more performant formula.
// In cooperation with Hanno Schwalm (hanno@schwalm-bremen.de) and Luis Sanz Rodriguez this has been tuned for performance.

rpError rcd_demosaic(int width, int height, const float * const *rawData, float **red, float **green, float **blue, const unsigned cfarray[2][2], const std::function<bool(double)> &setProgCancel, std::size_t chunkSize, bool measure, bool multiThread, rpTileExecutor executor, void *executorContext, rpShouldCancel shouldCancel, void *cancelContext, unsigned testFault)
{
    BENCHFUN

    std::unique_ptr<StopWatch> stop;

    if (measure) {
        std::cout << "Demosaicing " << width << "x" << height << " image using rcd with " << chunkSize << " tiles per thread" << std::endl;
        stop.reset(new StopWatch("rcd demosaic"));
    }
    if (!validateBayerCfa(3, cfarray)) {
        return RP_WRONG_CFA;
    }

    rpError rc = RP_NO_ERROR;

    setProgCancel(0.0);
    
    constexpr int tileBorder = 9; // avoid tile-overlap errors
    constexpr int rcdBorder = 9;
    constexpr int tileSize = 194;
    constexpr int tileSizeN = tileSize - 2 * tileBorder;
    const int numTh = height / (tileSizeN) + ((height % (tileSizeN)) ? 1 : 0);
    const int numTw = width / (tileSizeN) + ((width % (tileSizeN)) ? 1 : 0);
    constexpr int w1 = tileSize, w2 = 2 * tileSize, w3 = 3 * tileSize, w4 = 4 * tileSize;
    //Tolerance to avoid dividing by zero
    constexpr float eps = 1e-5f;
    constexpr float epssq = 1e-10f;
    constexpr float scale = 65536.f;

    // Lightwell: tiles run as jobs of a synchronous executor instead of an
    // OpenMP region (multiThread is unused). A full tile reads only scratch
    // cells it wrote or cells no tile writes, which stay zero. A partial tile
    // also reads direction and colour-difference cells at its last computed
    // row and column that only an earlier, larger tile wrote, so its result
    // depends on the serial tiles before it. Every job is therefore a
    // contiguous run of the serial raster that starts at a full tile: up to
    // eight tiles of a full-height row, the row's partial right tiles staying
    // with their full predecessor, and a final job from the last full-height
    // row's last chunk to the end of the frame. Each job owns one fresh
    // scratch set and checks cancellation and shared error state before every
    // tile. Without an executor, or without a full tile, one job keeps the
    // original raster over the whole frame.
    const int fullRows = height >= tileSize ? (height - tileSize) / tileSizeN + 1 : 0;
    const int fullCols = width >= tileSize ? (width - tileSize) / tileSizeN + 1 : 0;
    constexpr int maxJobTiles = 8;
    int chunks = (numTw + maxJobTiles - 1) / maxJobTiles;
    if (chunks > 1 && (chunks - 1) * maxJobTiles > fullCols - 1) {
        --chunks;
    }
    const size_t tileCount = size_t(numTh) * size_t(numTw);
    const size_t jobs = executor && fullRows > 0 && fullCols > 0 ? size_t(fullRows) * size_t(chunks) : 1;
    std::atomic<rpError> tileError{RP_NO_ERROR};
    RcdWorkerCall call{
        [&](size_t job) {
    if (tileError.load(std::memory_order_relaxed) != RP_NO_ERROR) return;
    // Private adapter tests inject these failures without provoking
    // process-wide OOM or exposing a user-facing control.
    if (testFault == 1 && job == 0) {
        rpError expected = RP_NO_ERROR;
        tileError.compare_exchange_strong(expected, RP_MEMORY_ERROR);
        return;
    }
    if (testFault == 2 && job == 0) throw std::runtime_error("native tile test fault");
    std::unique_ptr<float, decltype(&free)> cfaBuffer((float*) calloc(tileSize * tileSize, sizeof(float)), &free);
    std::unique_ptr<float, decltype(&free)> rgbBuffer((float*) malloc(3 * tileSize * tileSize * sizeof(float)), &free);
    std::unique_ptr<float, decltype(&free)> vhBuffer((float*) calloc(tileSize * tileSize, sizeof(float)), &free);
    std::unique_ptr<float, decltype(&free)> pqBuffer((float*) calloc(tileSize * tileSize / 2, sizeof(float)), &free);
    std::unique_ptr<float, decltype(&free)> pBuffer((float*) calloc(tileSize * tileSize / 2, sizeof(float)), &free);
    std::unique_ptr<float, decltype(&free)> qBuffer((float*) calloc(tileSize * tileSize / 2, sizeof(float)), &free);
    float *const cfa = cfaBuffer.get();
    float (*const rgb)[tileSize * tileSize] = (float (*)[tileSize * tileSize]) rgbBuffer.get();
    float *const VH_Dir = vhBuffer.get();
    float *const PQ_Dir = pqBuffer.get();
    float *const lpf = PQ_Dir; // reuse buffer, they don't overlap in usage
    float *const P_CDiff_Hpf = pBuffer.get();
    float *const Q_CDiff_Hpf = qBuffer.get();
    if (!cfa || !rgb || !VH_Dir || !PQ_Dir || !P_CDiff_Hpf || !Q_CDiff_Hpf) {
        rpError expected = RP_NO_ERROR;
        tileError.compare_exchange_strong(expected, RP_MEMORY_ERROR);
        return;
    }
    const size_t jobRow = job / size_t(chunks);
    const size_t jobChunk = job % size_t(chunks);
    const size_t firstTile = jobs == 1 ? 0 : jobRow * numTw + jobChunk * maxJobTiles;
    const size_t endTile = jobs == 1 || job == jobs - 1 ? tileCount
                           : jobChunk == size_t(chunks) - 1 ? (jobRow + 1) * numTw
                           : jobRow * numTw + (jobChunk + 1) * maxJobTiles;
        for (size_t tile = firstTile; tile < endTile; ++tile) {
                if (tileError.load(std::memory_order_relaxed) != RP_NO_ERROR) return;
                if (shouldCancel && shouldCancel(cancelContext)) {
                    rpError expected = RP_NO_ERROR;
                    tileError.compare_exchange_strong(expected, RP_CANCELLED);
                    return;
                }
                const int tr = int(tile / numTw);
                const int tc = int(tile % numTw);
                const int rowStart = tr * tileSizeN;
                const int rowEnd = std::min(rowStart + tileSize, height);
                if(rowStart + rcdBorder == rowEnd - rcdBorder) {
                    continue;
                }
                const int colStart = tc * tileSizeN;
                const int colEnd = std::min(colStart + tileSize, width);
                if(colStart + rcdBorder == colEnd - rcdBorder) {
                    continue;
                }

                const int tileRows = std::min(rowEnd - rowStart, tileSize);
                const int tilecols = std::min(colEnd - colStart, tileSize);

                for (int row = rowStart; row < rowEnd; row++) {
                    const int c0 = fc(cfarray, row, colStart);
                    const int c1 = fc(cfarray, row, colStart + 1);
                    for (int col = colStart, indx = (row - rowStart) * tileSize; col < colEnd; ++col, ++indx) {
                        cfa[indx] = rgb[c0][indx] = rgb[c1][indx] = LIM01(rawData[row][col] / scale);
                    }
                }

                // Step 1: Find cardinal and diagonal interpolation directions
                float bufferV[3][tileSize - 8];

                // Step 1.1: Calculate the square of the vertical and horizontal color difference high pass filter
                for (int row = 3; row < std::min(tileRows - 3, 5); ++row) {
                    for (int col = 4, indx = row * tileSize + col; col < tilecols - 4; ++col, ++indx) {
                        bufferV[row - 3][col - 4] = SQR((cfa[indx - w3] - cfa[indx - w1] - cfa[indx + w1] + cfa[indx + w3]) - 3.f * (cfa[indx - w2] + cfa[indx + w2])  + 6.f * cfa[indx]);
                    }
                }

                // Step 1.2: Obtain the vertical and horizontal directional discrimination strength
                float bufferH[tileSize - 6] ALIGNED16;
                float* V0 = bufferV[0];
                float* V1 = bufferV[1];
                float* V2 = bufferV[2];
                for (int row = 4; row < tileRows - 4; ++row) {
                    for (int col = 3, indx = row * tileSize + col; col < tilecols - 3; ++col, ++indx) {
                        bufferH[col - 3] = SQR((cfa[indx -  3] - cfa[indx -  1] - cfa[indx +  1] + cfa[indx +  3]) - 3.f * (cfa[indx -  2] + cfa[indx +  2]) + 6.f * cfa[indx]);
                    }
                    for (int col = 4, indx = (row + 1) * tileSize + col; col < tilecols - 4; ++col, ++indx) {
                        V2[col - 4] = SQR((cfa[indx - w3] - cfa[indx - w1] - cfa[indx + w1] + cfa[indx + w3]) - 3.f * (cfa[indx - w2] + cfa[indx + w2])  + 6.f * cfa[indx]);
                    }
                    for (int col = 4, indx = row * tileSize + col; col < tilecols - 4; ++col, ++indx) {

                        float V_Stat = std::max(epssq, V0[col - 4] + V1[col - 4] + V2[col - 4]);
                        float H_Stat = std::max(epssq, bufferH[col -  4] + bufferH[col - 3] + bufferH[col -  2]);

                        VH_Dir[indx] = V_Stat / (V_Stat + H_Stat);
                    }
                    // rotate pointers from row0, row1, row2 to row1, row2, row0
                    std::swap(V0, V2);
                    std::swap(V0, V1);
                }

                // Step 2: Low pass filter incorporating green, red and blue local samples from the raw data
                for (int row = 2; row < tileRows - 2; ++row) {
                    for (int col = 2 + (fc(cfarray, row, 0) & 1), indx = row * tileSize + col, lpindx = indx / 2; col < tilecols - 2; col += 2, indx += 2, ++lpindx) {
                        lpf[lpindx] = cfa[indx] +
                                      0.5f * (cfa[indx - w1] + cfa[indx + w1] + cfa[indx - 1] + cfa[indx + 1]) +
                                      0.25f * (cfa[indx - w1 - 1] + cfa[indx - w1 + 1] + cfa[indx + w1 - 1] + cfa[indx + w1 + 1]);
                    }
                }

                // Step 3: Populate the green channel at blue and red CFA positions
                for (int row = 4; row < tileRows - 4; ++row) {
                    for (int col = 4 + (fc(cfarray, row, 0) & 1), indx = row * tileSize + col, lpindx = indx / 2; col < tilecols - 4; col += 2, indx += 2, ++lpindx) {
                        // Cardinal gradients
                        const float cfai = cfa[indx];
                        const float N_Grad = eps + (std::fabs(cfa[indx - w1] - cfa[indx + w1]) + std::fabs(cfai - cfa[indx - w2])) + (std::fabs(cfa[indx - w1] - cfa[indx - w3]) + std::fabs(cfa[indx - w2] - cfa[indx - w4]));
                        const float S_Grad = eps + (std::fabs(cfa[indx - w1] - cfa[indx + w1]) + std::fabs(cfai - cfa[indx + w2])) + (std::fabs(cfa[indx + w1] - cfa[indx + w3]) + std::fabs(cfa[indx + w2] - cfa[indx + w4]));
                        const float W_Grad = eps + (std::fabs(cfa[indx -  1] - cfa[indx +  1]) + std::fabs(cfai - cfa[indx -  2])) + (std::fabs(cfa[indx -  1] - cfa[indx -  3]) + std::fabs(cfa[indx -  2] - cfa[indx -  4]));
                        const float E_Grad = eps + (std::fabs(cfa[indx -  1] - cfa[indx +  1]) + std::fabs(cfai - cfa[indx +  2])) + (std::fabs(cfa[indx +  1] - cfa[indx +  3]) + std::fabs(cfa[indx +  2] - cfa[indx +  4]));

                        // Cardinal pixel estimations
                        const float lpfi = lpf[lpindx];
                        const float N_Est = cfa[indx - w1] * (lpfi + lpfi) / (eps + lpfi + lpf[lpindx - w1]);
                        const float S_Est = cfa[indx + w1] * (lpfi + lpfi) / (eps + lpfi + lpf[lpindx + w1]);
                        const float W_Est = cfa[indx -  1] * (lpfi + lpfi) / (eps + lpfi + lpf[lpindx -  1]);
                        const float E_Est = cfa[indx +  1] * (lpfi + lpfi) / (eps + lpfi + lpf[lpindx +  1]);

                        // Vertical and horizontal estimations
                        const float V_Est = (S_Grad * N_Est + N_Grad * S_Est) / (N_Grad + S_Grad);
                        const float H_Est = (W_Grad * E_Est + E_Grad * W_Est) / (E_Grad + W_Grad);

                        // G@B and G@R interpolation
                        // Refined vertical and horizontal local discrimination
                        const float VH_Central_Value = VH_Dir[indx];
                        const float VH_Neighbourhood_Value = 0.25f * ((VH_Dir[indx - w1 - 1] + VH_Dir[indx - w1 + 1]) + (VH_Dir[indx + w1 - 1] + VH_Dir[indx + w1 + 1]));

                        const float VH_Disc = std::fabs(0.5f - VH_Central_Value) < std::fabs(0.5f - VH_Neighbourhood_Value) ? VH_Neighbourhood_Value : VH_Central_Value;
                        rgb[1][indx] = intp(VH_Disc, H_Est, V_Est);
                    }
                }

                /**
                * STEP 4: Populate the red and blue channels
                */

                // Step 4.0: Calculate the square of the P/Q diagonals color difference high pass filter
                for (int row = 3; row < tileRows - 3; ++row) {
                    for (int col = 3, indx = row * tileSize + col, indx2 = indx / 2; col < tilecols - 3; col+=2, indx+=2, indx2++ ) {
                        P_CDiff_Hpf[indx2] = SQR((cfa[indx - w3 - 3] - cfa[indx - w1 - 1] - cfa[indx + w1 + 1] + cfa[indx + w3 + 3]) - 3.f * (cfa[indx - w2 - 2] + cfa[indx + w2 + 2]) + 6.f * cfa[indx]);
                        Q_CDiff_Hpf[indx2] = SQR((cfa[indx - w3 + 3] - cfa[indx - w1 + 1] - cfa[indx + w1 - 1] + cfa[indx + w3 - 3]) - 3.f * (cfa[indx - w2 + 2] + cfa[indx + w2 - 2]) + 6.f * cfa[indx]);
                    }
                }

                // Step 4.1: Obtain the P/Q diagonals directional discrimination strength
                for (int row = 4; row < tileRows - 4; ++row) {
                    for (int col = 4 + (fc(cfarray, row, 0) & 1), indx = row * tileSize + col, indx2 = indx / 2, indx3 = (indx - w1 - 1) / 2, indx4 = (indx + w1 - 1) / 2; col < tilecols - 4; col += 2, indx += 2, indx2++, indx3++, indx4++ ) {
                        float P_Stat = std::max(epssq, P_CDiff_Hpf[indx3] + P_CDiff_Hpf[indx2] + P_CDiff_Hpf[indx4 + 1]);
                        float Q_Stat = std::max(epssq, Q_CDiff_Hpf[indx3 + 1] + Q_CDiff_Hpf[indx2] + Q_CDiff_Hpf[indx4]);
                        PQ_Dir[indx2] = P_Stat / (P_Stat + Q_Stat);
                    }
                }

                // Step 4.2: Populate the red and blue channels at blue and red CFA positions
                for (int row = 4; row < tileRows - 4; ++row) {
                    for (int col = 4 + (fc(cfarray, row, 0) & 1), indx = row * tileSize + col, c = 2 - fc(cfarray, row, col), pqindx = indx / 2, pqindx2 = (indx - w1 - 1) / 2, pqindx3 = (indx + w1 - 1) / 2; col < tilecols - 4; col += 2, indx += 2, ++pqindx, ++pqindx2, ++pqindx3) {

                        // Refined P/Q diagonal local discrimination
                        float PQ_Central_Value   = PQ_Dir[pqindx];
                        float PQ_Neighbourhood_Value = 0.25f * (PQ_Dir[pqindx2] + PQ_Dir[pqindx2 + 1] + PQ_Dir[pqindx3] + PQ_Dir[pqindx3 + 1]);

                        float PQ_Disc = (std::fabs(0.5f - PQ_Central_Value) < std::fabs(0.5f - PQ_Neighbourhood_Value)) ? PQ_Neighbourhood_Value : PQ_Central_Value;

                        // Diagonal gradients
                        float NW_Grad = eps + std::fabs(rgb[c][indx - w1 - 1] - rgb[c][indx + w1 + 1]) + std::fabs(rgb[c][indx - w1 - 1] - rgb[c][indx - w3 - 3]) + std::fabs(rgb[1][indx] - rgb[1][indx - w2 - 2]);
                        float NE_Grad = eps + std::fabs(rgb[c][indx - w1 + 1] - rgb[c][indx + w1 - 1]) + std::fabs(rgb[c][indx - w1 + 1] - rgb[c][indx - w3 + 3]) + std::fabs(rgb[1][indx] - rgb[1][indx - w2 + 2]);
                        float SW_Grad = eps + std::fabs(rgb[c][indx - w1 + 1] - rgb[c][indx + w1 - 1]) + std::fabs(rgb[c][indx + w1 - 1] - rgb[c][indx + w3 - 3]) + std::fabs(rgb[1][indx] - rgb[1][indx + w2 - 2]);
                        float SE_Grad = eps + std::fabs(rgb[c][indx - w1 - 1] - rgb[c][indx + w1 + 1]) + std::fabs(rgb[c][indx + w1 + 1] - rgb[c][indx + w3 + 3]) + std::fabs(rgb[1][indx] - rgb[1][indx + w2 + 2]);

                        // Diagonal colour differences
                        float NW_Est = rgb[c][indx - w1 - 1] - rgb[1][indx - w1 - 1];
                        float NE_Est = rgb[c][indx - w1 + 1] - rgb[1][indx - w1 + 1];
                        float SW_Est = rgb[c][indx + w1 - 1] - rgb[1][indx + w1 - 1];
                        float SE_Est = rgb[c][indx + w1 + 1] - rgb[1][indx + w1 + 1];

                        // P/Q estimations
                        float P_Est = (NW_Grad * SE_Est + SE_Grad * NW_Est) / (NW_Grad + SE_Grad);
                        float Q_Est = (NE_Grad * SW_Est + SW_Grad * NE_Est) / (NE_Grad + SW_Grad);

                        // R@B and B@R interpolation
                        rgb[c][indx] = rgb[1][indx] + intp(PQ_Disc, Q_Est, P_Est);
                    }
                }

                // Step 4.3: Populate the red and blue channels at green CFA positions
                for (int row = 4; row < tileRows - 4; ++row) {
                    for (int col = 4 + (fc(cfarray, row, 1) & 1), indx = row * tileSize + col; col < tilecols - 4; col += 2, indx += 2) {

                        // Refined vertical and horizontal local discrimination
                        float VH_Central_Value = VH_Dir[indx];
                        float VH_Neighbourhood_Value = 0.25f * ((VH_Dir[indx - w1 - 1] + VH_Dir[indx - w1 + 1]) + (VH_Dir[indx + w1 - 1] + VH_Dir[indx + w1 + 1]));

                        float VH_Disc = (std::fabs(0.5f - VH_Central_Value) < std::fabs(0.5f - VH_Neighbourhood_Value)) ? VH_Neighbourhood_Value : VH_Central_Value;
                        float rgb1 = rgb[1][indx];
                        float N1 = eps + std::fabs(rgb1 - rgb[1][indx - w2]);
                        float S1 = eps + std::fabs(rgb1 - rgb[1][indx + w2]);
                        float W1 = eps + std::fabs(rgb1 - rgb[1][indx -  2]);
                        float E1 = eps + std::fabs(rgb1 - rgb[1][indx +  2]);

                        float rgb1mw1 = rgb[1][indx - w1];
                        float rgb1pw1 = rgb[1][indx + w1];
                        float rgb1m1 = rgb[1][indx - 1];
                        float rgb1p1 = rgb[1][indx + 1];
                        for (int c = 0; c <= 2; c += 2) {
                            // Cardinal gradients
                            float SNabs = std::fabs(rgb[c][indx - w1] - rgb[c][indx + w1]);
                            float EWabs = std::fabs(rgb[c][indx -  1] - rgb[c][indx +  1]);
                            float N_Grad = N1 + SNabs + std::fabs(rgb[c][indx - w1] - rgb[c][indx - w3]);
                            float S_Grad = S1 + SNabs + std::fabs(rgb[c][indx + w1] - rgb[c][indx + w3]);
                            float W_Grad = W1 + EWabs + std::fabs(rgb[c][indx -  1] - rgb[c][indx -  3]);
                            float E_Grad = E1 + EWabs + std::fabs(rgb[c][indx +  1] - rgb[c][indx +  3]);

                            // Cardinal colour differences
                            float N_Est = rgb[c][indx - w1] - rgb1mw1;
                            float S_Est = rgb[c][indx + w1] - rgb1pw1;
                            float W_Est = rgb[c][indx -  1] - rgb1m1;
                            float E_Est = rgb[c][indx +  1] - rgb1p1;

                            // Vertical and horizontal estimations
                            float V_Est = (N_Grad * S_Est + S_Grad * N_Est) / (N_Grad + S_Grad);
                            float H_Est = (E_Grad * W_Est + W_Grad * E_Est) / (E_Grad + W_Grad);

                            // R@G and B@G interpolation
                            rgb[c][indx] = rgb1 + intp(VH_Disc, H_Est, V_Est);
                        }
                    }
                }

                // For the outermost tiles in all directions we can use a smaller border margin
                const int firstVertical = rowStart + ((tr == 0) ? rcdBorder : tileBorder);
                const int lastVertical = rowEnd - ((tr == numTh - 1) ? rcdBorder : tileBorder);
                const int firstHorizontal = colStart + ((tc == 0) ? rcdBorder : tileBorder);
                const int lastHorizontal =  colEnd - ((tc == numTw - 1) ? rcdBorder : tileBorder);
                for (int row = firstVertical; row < lastVertical; ++row) {
                    for (int col = firstHorizontal; col < lastHorizontal; ++col) {
                        int idx = (row - rowStart) * tileSize + col - colStart ;
                        red[row][col] = std::max(0.f, rgb[0][idx] * scale);
                        green[row][col] = std::max(0.f, rgb[1][idx] * scale);
                        blue[row][col] = std::max(0.f, rgb[2][idx] * scale);
                    }
                }

        }
        },
        &tileError
    };
    // The executor joins every job before returning. Serial callers run the
    // same job function once over the whole raster.
    const int result = executor
                           ? executor(executorContext, jobs, run_rcd_worker, &call)
                           : (run_rcd_worker(&call, 0), 0);
    if (result == 2) return RP_CANCELLED;
    if (result != 0) return RP_WORKER_ERROR;
    rc = tileError.load(std::memory_order_relaxed);
    if (rc != RP_NO_ERROR) return rc;
    if (shouldCancel && shouldCancel(cancelContext)) return RP_CANCELLED;
    rc = bayerborder_demosaic(width, height, rcdBorder, rawData, red, green, blue, cfarray);

    setProgCancel(1.0);

    return rc;
}

