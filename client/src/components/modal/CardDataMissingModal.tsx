interface CardDataMissingModalProps {
  onContinue: () => void;
}

export function CardDataMissingModal({ onContinue }: CardDataMissingModalProps) {
  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center">
      <div className="absolute inset-0 bg-black/70" />
      <div className="relative z-10 max-w-md rounded-xl bg-gray-900 p-8 shadow-2xl ring-1 ring-gray-700">
        <h2 className="mb-3 text-xl font-bold text-white">Card Data Missing</h2>
        <p className="mb-4 text-sm text-gray-300">
          <code className="rounded bg-gray-800 px-1 py-0.5 text-amber-400">card-data.json</code>{" "}
          was not found. The game needs card definitions to play with real cards.
        </p>
        <p className="mb-3 text-sm text-gray-400">
          Generate it by running:
        </p>
        <pre className="mb-6 overflow-x-auto rounded-lg bg-gray-950 p-3 text-sm text-emerald-400">
          cargo run --bin card_data_export
        </pre>
        <p className="mb-1 text-xs text-gray-500">
          Then place the output file in{" "}
          <code className="text-gray-400">client/public/card-data.json</code> and reload.
        </p>
        <div className="mt-6 text-center">
          <button
            onClick={onContinue}
            className="text-sm text-gray-500 underline hover:text-gray-300"
          >
            Continue anyway
          </button>
        </div>
      </div>
    </div>
  );
}
