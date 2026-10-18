import { DatePicker } from 'antd'
import dayjs, { type Dayjs } from 'dayjs'

type UsageRangePickerProps = {
  dates: [Dayjs, Dayjs]
  onChange: (dates: [Dayjs, Dayjs]) => void
}

export default function UsageRangePicker({
  dates,
  onChange,
}: UsageRangePickerProps) {
  const rangePresets = [
    {
      label: '今天',
      value: [dayjs().startOf('day'), dayjs().endOf('day')] as [Dayjs, Dayjs],
    },
    {
      label: '近 7 天',
      value: [dayjs().subtract(6, 'day').startOf('day'), dayjs().endOf('day')] as [
        Dayjs,
        Dayjs,
      ],
    },
    {
      label: '近 14 天',
      value: [
        dayjs().subtract(13, 'day').startOf('day'),
        dayjs().endOf('day'),
      ] as [Dayjs, Dayjs],
    },
    {
      label: '近 30 天',
      value: [
        dayjs().subtract(29, 'day').startOf('day'),
        dayjs().endOf('day'),
      ] as [Dayjs, Dayjs],
    },
  ]

  return (
    <DatePicker.RangePicker
      allowClear={false}
      value={dates}
      presets={rangePresets}
      disabledDate={(current) => Boolean(current && current > dayjs().endOf('day'))}
      onChange={(value) => {
        if (value?.[0] && value[1]) onChange(value as [Dayjs, Dayjs])
      }}
    />
  )
}
